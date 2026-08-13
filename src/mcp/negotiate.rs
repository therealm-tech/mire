//! Settling which revision a server actually speaks.
//!
//! # Why there is a ladder rather than a question
//!
//! The obvious design is to ask. `server/discover` does exactly that: it answers
//! `protocolVersions` and the client picks. The catch is that `server/discover`
//! is itself a method of `2026-07-28` — the very revision we are trying to find
//! out whether the server speaks. Asking an older server what it speaks fails in
//! the same way as asking it for anything else, which is the failure that started
//! all this.
//!
//! So the probe is newest-first and falls through:
//!
//! 1. **`server/discover`.** A server on the newest revision answers with every
//!    version it speaks, and we take the best one we share.
//! 2. **`initialize`.** The older revisions' own negotiation: the client proposes
//!    a version, the server replies with the one it will actually use — which may
//!    be older than what was proposed, and that is a success, not a downgrade to
//!    be suspicious of.
//! 3. **Neither answered: assume the newest.** Not a guess for its own sake —
//!    it is precisely what `mire` did before it could negotiate at all, and
//!    `server/discover` is a method a perfectly good `2026-07-28` server is free
//!    not to implement. Failing here would break servers that work today in order
//!    to report a problem they do not have. The request goes out as it always
//!    did, and if it fails, it fails with its own error rather than one invented
//!    by the probe in front of it.
//!
//! Worst case that is two round trips, once per server, then cached.
//!
//! # A harness may not negotiate quietly
//!
//! Everywhere else, a client that transparently settles on an older protocol is
//! being helpful. Here it would be destroying the measurement: the entire point
//! of `mire` is to tell you what your endpoint does, so the revision that ended
//! up in force rides on the `GET /api/mcp/{name}/tools` response next to the
//! tools it produced, and says which rung of the ladder settled it.
//!
//! And [`McpServer::protocol_version`](super::McpServer) pins it outright, which
//! turns the version back into an input you control: pinning a revision the
//! server refuses is a legitimate thing to want to observe.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tracing::{debug, warn};

use super::{McpClient, McpCredentials, McpError, Revision};

/// How the revision in use was arrived at.
///
/// Reported rather than kept internal: "it negotiated" and "you pinned it" are
/// different facts about a run, and a test result that conflates them is worth
/// less than one that does not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Settled {
    /// `protocol_version:` in `mcp.yaml`. Nothing was asked.
    Pinned,
    /// The server listed its versions through `server/discover`.
    Discovered,
    /// The server chose it in its `initialize` reply.
    Handshake,
    /// Nothing would say, so the newest revision was assumed.
    ///
    /// Reported rather than hidden: a run that worked because a guess happened to
    /// be right is a different fact from one that worked because both ends agreed,
    /// and only one of them stays true next week.
    Assumed,
}

/// The settled protocol state for one server.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// The revision both sides ended up on.
    pub revision: Revision,
    /// How that was decided.
    pub settled: Settled,
    /// `Mcp-Session-Id`, when the server issued one.
    ///
    /// Only the handshaking revisions have sessions, and even there a server is
    /// free not to issue one — a stateless server on an older revision is a
    /// perfectly ordinary thing to point `mire` at.
    #[serde(skip)]
    pub id: Option<String>,
}

impl Session {
    /// A sessionless state, for the revision that has no handshake.
    #[must_use]
    pub const fn sessionless(revision: Revision, settled: Settled) -> Self {
        Self {
            revision,
            settled,
            id: None,
        }
    }

    /// Whether this state carries a server-issued session identifier.
    #[must_use]
    pub const fn has_id(&self) -> bool {
        self.id.is_some()
    }
}

/// Runs the ladder for one server.
///
/// # Errors
///
/// [`McpError::NoCommonRevision`] when the server named its versions and none of
/// them is ours — the one failure worth telling apart from every other `400`.
/// Nothing else: a server that answers no probe at all is not an error here, it
/// is rung three.
pub async fn negotiate(
    client: &McpClient,
    credentials: &McpCredentials<'_>,
) -> Result<Session, McpError> {
    let server = &client.server().name;

    // A pin is a statement, not a preference: it skips both probes, including the
    // handshake-free path, so that pinning something the server refuses produces
    // the refusal you asked to see rather than a silent fallback.
    if let Some(revision) = client.server().protocol_version {
        debug!(server = %server, %revision, "protocol revision pinned, not negotiating");
        return settle(client, revision, Settled::Pinned, credentials).await;
    }

    let discovery = match discover(client, credentials).await {
        Ok(revision) => {
            debug!(server = %server, %revision, "revision discovered");
            return settle(client, revision, Settled::Discovered, credentials).await;
        }
        // A server with no revision in common is a finished answer, not a rung to
        // fall off: it told us what it speaks and we cannot speak it. Falling
        // through to `initialize` would replace that with a vaguer error.
        Err(error @ McpError::NoCommonRevision { .. }) => return Err(error),
        Err(error) => {
            debug!(server = %server, %error, "`server/discover` got us nowhere, trying `initialize`");
            error
        }
    };

    match handshake(client, Revision::LATEST_LEGACY, credentials).await {
        Ok(session) => {
            debug!(
                server = %server,
                revision = %session.revision,
                session = session.has_id(),
                "revision settled by handshake"
            );
            Ok(session)
        }
        // The server named its versions and we share none: a finished answer, and
        // assuming one it just told us it does not speak would be absurd.
        Err(error @ McpError::NoCommonRevision { .. }) => Err(error),
        Err(handshake) => {
            // Rung three. Both probes are optional methods as far as any given
            // server is concerned, so their silence says nothing about whether
            // `tools/list` works — and that is the call the user actually made.
            warn!(
                server = %server,
                revision = %Revision::LATEST,
                "neither probe settled a revision, assuming the newest: \
                 `server/discover` said: {discovery}; `initialize` said: {handshake}"
            );
            Ok(Session::sessionless(Revision::LATEST, Settled::Assumed))
        }
    }
}

/// Brings a chosen revision into a usable state, handshaking if it needs one.
async fn settle(
    client: &McpClient,
    revision: Revision,
    settled: Settled,
    credentials: &McpCredentials<'_>,
) -> Result<Session, McpError> {
    if !revision.handshakes() {
        return Ok(Session::sessionless(revision, settled));
    }

    let mut session = handshake(client, revision, credentials).await?;
    // How we got here is the more informative fact. A revision that was pinned
    // and then confirmed by a handshake is still pinned.
    session.settled = settled;
    Ok(session)
}

/// Rung one: ask the newest revision's own discovery method.
async fn discover(
    client: &McpClient,
    credentials: &McpCredentials<'_>,
) -> Result<Revision, McpError> {
    let result = client
        .exchange(
            &Session::sessionless(Revision::LATEST, Settled::Discovered),
            "server/discover",
            json!({}),
            None,
            &[],
            credentials,
        )
        .await?
        .result;

    let offered: Vec<String> = result
        .get("protocolVersions")
        .and_then(Value::as_array)
        .map(|versions| {
            versions
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    if offered.is_empty() {
        return Err(McpError::Protocol {
            server: client.server().name.clone(),
            message: "`server/discover` listed no `protocolVersions`".to_owned(),
        });
    }

    choose(&offered).ok_or_else(|| McpError::NoCommonRevision {
        server: client.server().name.clone(),
        ours: ours(),
        theirs: offered.join(", "),
    })
}

/// Rung two: the handshake, which is the older revisions' own negotiation.
///
/// What comes back may be older than what went out. That is the mechanism doing
/// its job — the server is telling us what it will speak — so it is accepted as
/// long as we speak it.
async fn handshake(
    client: &McpClient,
    proposed: Revision,
    credentials: &McpCredentials<'_>,
) -> Result<Session, McpError> {
    let params = json!({
        "protocolVersion": proposed.as_str(),
        "capabilities": {},
        "clientInfo": {"name": "mire", "version": env!("CARGO_PKG_VERSION")},
    });

    let exchange = client
        .exchange(
            &Session::sessionless(proposed, Settled::Handshake),
            "initialize",
            params,
            None,
            &[],
            credentials,
        )
        .await?;

    let Some(agreed) = exchange
        .result
        .get("protocolVersion")
        .and_then(Value::as_str)
    else {
        return Err(McpError::Protocol {
            server: client.server().name.clone(),
            message: "`initialize` answered without a `protocolVersion`".to_owned(),
        });
    };

    let revision = agreed
        .parse::<Revision>()
        .map_err(|_| McpError::NoCommonRevision {
            server: client.server().name.clone(),
            ours: ours(),
            theirs: agreed.to_owned(),
        })?;

    if revision != proposed {
        debug!(
            server = %client.server().name,
            %proposed,
            chose = %revision,
            "the server chose a different revision, which is the handshake working"
        );
    }

    let session = Session {
        revision,
        settled: Settled::Handshake,
        id: exchange.session_id,
    };

    // The handshake is only over once the server has been told so. A server that
    // rejects the notification has still handshaked, so this warns rather than
    // failing the negotiation — the next request will say so far more usefully.
    if let Err(error) = client
        .notify(
            &session,
            "notifications/initialized",
            json!({}),
            credentials,
        )
        .await
    {
        warn!(
            server = %client.server().name,
            %error,
            "`notifications/initialized` was refused; continuing, the next request will tell us more"
        );
    }

    Ok(session)
}

/// The best revision we share with a server that listed these.
///
/// Unparseable entries are simply not ours, which is the same answer as a version
/// we have never heard of — there is nothing to report about `"banana"` beyond
/// its absence from the intersection.
#[must_use]
pub fn choose(offered: &[String]) -> Option<Revision> {
    offered
        .iter()
        .filter_map(|version| version.parse::<Revision>().ok())
        .max()
}

/// What this build speaks, newest first, for an error message.
fn ours() -> String {
    Revision::ALL
        .iter()
        .rev()
        .map(|revision| revision.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn offered(versions: &[&str]) -> Vec<String> {
        versions.iter().map(|v| (*v).to_owned()).collect()
    }

    #[test]
    fn the_best_shared_revision_wins_regardless_of_the_order_offered() {
        assert_eq!(
            choose(&offered(&["2025-03-26", "2026-07-28", "2025-06-18"])),
            Some(Revision::V20260728)
        );
        // Newest-we-share, not newest-they-have and not first-listed.
        assert_eq!(
            choose(&offered(&["2025-06-18", "2025-03-26"])),
            Some(Revision::V20250618)
        );
    }

    #[test]
    fn a_version_we_do_not_know_is_simply_not_in_the_intersection() {
        // Including one from the future: unknown is unknown, in both directions.
        assert_eq!(
            choose(&offered(&["2027-01-01", "2025-03-26"])),
            Some(Revision::V20250326)
        );
        assert_eq!(choose(&offered(&["2027-01-01", "banana"])), None);
        assert_eq!(choose(&[]), None);
    }

    #[test]
    fn what_this_build_speaks_reads_newest_first() {
        assert_eq!(ours(), "2026-07-28, 2025-06-18, 2025-03-26");
    }

    #[test]
    fn a_sessionless_state_carries_no_identifier() {
        let session = Session::sessionless(Revision::LATEST, Settled::Discovered);
        assert!(!session.has_id());
        assert_eq!(session.revision, Revision::LATEST);
    }
}
