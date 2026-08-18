//! The one place that builds an HTTP client and sends a request.
//!
//! Single entry point on purpose: "a credential never survives a cross-host
//! redirect" is an invariant, and an invariant with two implementations is a bug
//! waiting for a quiet afternoon.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use reqwest::{Client, redirect};
use tracing::debug;

use crate::render::RenderedRequest;

/// Redirects followed before giving up. Generous enough for a gateway chain,
/// small enough that a loop is reported rather than hung on.
const MAX_REDIRECTS: usize = 5;

/// Client tuning that comes from the command line.
#[derive(Debug, Clone, Default)]
pub struct TransportOptions {
    /// PEM bundle of extra roots to trust, for an internal CA.
    pub ca_bundle: Option<PathBuf>,
}

/// Builds the shared HTTP client.
///
/// # Errors
///
/// Fails if the CA bundle cannot be read or parsed, or if the TLS backend cannot
/// be initialised.
pub fn build_client(options: &TransportOptions) -> Result<Client, TransportError> {
    let mut builder = Client::builder()
        // reqwest drops `Authorization`, `Cookie` and `Proxy-Authorization` when a
        // redirect crosses scheme, host or port. Following redirects at all is
        // still a choice, so cap it and keep the behaviour under test.
        .redirect(redirect::Policy::limited(MAX_REDIRECTS))
        .user_agent(concat!("mire/", env!("CARGO_PKG_VERSION")));

    if let Some(path) = &options.ca_bundle {
        for certificate in read_ca_bundle(path)? {
            builder = builder.add_root_certificate(certificate);
        }
        debug!(path = %path.display(), "custom CA bundle loaded");
    }

    builder.build().map_err(TransportError::Build)
}

fn read_ca_bundle(path: &Path) -> Result<Vec<reqwest::Certificate>, TransportError> {
    let pem = std::fs::read(path).map_err(|source| TransportError::CaBundleRead {
        path: path.to_owned(),
        source,
    })?;
    reqwest::Certificate::from_pem_bundle(&pem).map_err(|source| TransportError::CaBundleParse {
        path: path.to_owned(),
        source,
    })
}

/// A response, read to the end.
#[derive(Debug, Clone)]
pub struct RawResponse {
    /// HTTP status.
    pub status: u16,
    /// Response headers.
    pub headers: BTreeMap<String, String>,
    /// Body as text. Not parsed here: a non-JSON body is a finding, not a crash.
    pub body: String,
    /// Wall-clock time from send to full body.
    pub latency: Duration,
}

/// Sends a rendered request and reads the whole response.
///
/// # Errors
///
/// Returns [`TransportError::Timeout`] when the profile's timeout elapses, and
/// [`TransportError::Send`] for connection, TLS and protocol failures.
pub async fn send(
    client: &Client,
    request: &RenderedRequest,
    timeout: Duration,
) -> Result<RawResponse, TransportError> {
    let started = Instant::now();

    let response = client
        .request(request.method.into(), request.url.clone())
        .headers(request.headers.clone())
        .body(request.body.clone())
        .timeout(timeout)
        .send()
        .await
        .map_err(|error| classify(error, timeout))?;

    let status = response.status().as_u16();
    let headers = header_map(&response);

    let body = response
        .text()
        .await
        .map_err(|error| classify(error, timeout))?;

    Ok(RawResponse {
        status,
        headers,
        body,
        latency: started.elapsed(),
    })
}

/// A response whose head has arrived and whose body has not.
///
/// The split is the whole point of streaming: the status and headers are known
/// long before the last token, and a caller that wants to show a `401` — or start
/// the clock on time-to-first-token — cannot wait for the body to find out.
pub struct OpenResponse {
    /// HTTP status.
    pub status: u16,
    /// Response headers.
    pub headers: BTreeMap<String, String>,
    /// `content-type`, pulled out because the framing is detected from it.
    pub content_type: Option<String>,
    /// When the request went out, so every timing is measured from one origin.
    pub started: Instant,
    inner: reqwest::Response,
    timeout: Duration,
}

impl OpenResponse {
    /// Reads the body as a stream of text chunks, calling `on_chunk` for each.
    ///
    /// Text rather than bytes: every framing here is line-oriented UTF-8, and a
    /// multi-byte character split across two network reads is reassembled by the
    /// caller's buffer rather than being decoded twice.
    ///
    /// # Errors
    ///
    /// Fails on a read error or the profile's timeout. Bytes already delivered
    /// stay delivered — a stream that dies halfway is a finding, and the caller
    /// keeps what arrived.
    pub async fn read(
        mut self,
        mut on_chunk: impl FnMut(&str, Instant),
    ) -> Result<(), TransportError> {
        // A lone continuation byte at the end of a network read is not an error,
        // it is the next read's problem. Keeping the tail here is what makes a
        // multi-byte character survive an unlucky split.
        let mut tail: Vec<u8> = Vec::new();

        while let Some(bytes) = self
            .inner
            .chunk()
            .await
            .map_err(|error| classify(error, self.timeout))?
        {
            let at = Instant::now();
            tail.extend_from_slice(&bytes);
            let text = match std::str::from_utf8(&tail) {
                Ok(text) => {
                    let owned = text.to_owned();
                    tail.clear();
                    owned
                }
                Err(error) => {
                    let valid = error.valid_up_to();
                    // `from_utf8` just told us this prefix is valid.
                    let owned = String::from_utf8_lossy(&tail[..valid]).into_owned();
                    tail.drain(..valid);
                    owned
                }
            };
            if !text.is_empty() {
                on_chunk(&text, at);
            }
        }

        if !tail.is_empty() {
            // Trailing bytes that never became a character: the stream was cut
            // mid-sequence. Hand them over rather than pretending they were not
            // sent.
            on_chunk(&String::from_utf8_lossy(&tail), Instant::now());
        }

        Ok(())
    }
}

impl std::fmt::Debug for OpenResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenResponse")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .finish_non_exhaustive()
    }
}

/// Sends a rendered request and returns as soon as the head is in.
///
/// # Errors
///
/// Same failures as [`send`], minus the ones that can only happen while reading
/// the body — those surface from [`OpenResponse::read`].
pub async fn open(
    client: &Client,
    request: &RenderedRequest,
    timeout: Duration,
) -> Result<OpenResponse, TransportError> {
    let started = Instant::now();

    let response = client
        .request(request.method.into(), request.url.clone())
        .headers(request.headers.clone())
        .body(request.body.clone())
        .timeout(timeout)
        .send()
        .await
        .map_err(|error| classify(error, timeout))?;

    let headers = header_map(&response);
    let content_type = headers.get("content-type").cloned();

    debug!(
        status = response.status().as_u16(),
        head_ms = started.elapsed().as_millis(),
        "response head"
    );

    Ok(OpenResponse {
        status: response.status().as_u16(),
        headers,
        content_type,
        started,
        inner: response,
        timeout,
    })
}

fn header_map(response: &reqwest::Response) -> BTreeMap<String, String> {
    response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or("<non-utf8>").to_owned(),
            )
        })
        .collect()
}

fn classify(error: reqwest::Error, timeout: Duration) -> TransportError {
    if error.is_timeout() {
        TransportError::Timeout {
            after_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        }
    } else {
        TransportError::Send {
            message: explain(&error),
            source: Box::new(error),
        }
    }
}

/// Why a request never produced a response, in words somebody can act on.
///
/// `reqwest` stops its own `Display` at `error sending request for url (…)`. The
/// half that says *why* — a name that does not resolve, a refused connection, a
/// certificate nobody here signed — is in the source chain, and nothing renders
/// that by default. Printed next to the URL it repeats, that message tells the
/// reader precisely what they were already looking at.
///
/// So: a first clause naming the kind of failure, then the causes it was hiding.
/// One place, because every outbound call in this process — a profile's request,
/// an MCP server, a hook, a token exchange — fails the same way and a reader
/// should not have to learn two vocabularies for it.
#[must_use]
pub fn explain(error: &reqwest::Error) -> String {
    // `is_timeout` and `is_connect` both walk the chain, so the order here is
    // only about which word is the more useful one: a connect that ran out of
    // time is a timeout first.
    let lead = if error.is_timeout() {
        "no answer within the timeout"
    } else if error.is_connect() {
        "could not reach the endpoint"
    } else if error.is_redirect() {
        "too many redirects"
    } else if error.is_body() || error.is_decode() {
        "the answer could not be read"
    } else if error.is_builder() {
        "the request could not be built"
    } else {
        "the request could not be sent"
    };

    match causes(error).as_slice() {
        [] => lead.to_owned(),
        causes => format!("{lead}: {}", causes.join(": ")),
    }
}

/// The source chain, outermost first, minus everything it repeats.
///
/// `hyper` nests each cause inside the next one's text — `tcp connect error:
/// dns error: …` — so walking the chain naively says the same thing three times
/// and buries the sentence that matters. A cause the previous one already
/// contains is dropped, and so is `client error (Connect)`, which is `hyper`
/// restating the kind [`explain`] has just named in plain words.
///
/// `&dyn Error` because that is what [`std::error::Error::source`] hands back;
/// there is no static shape to a chain of anonymous boxed errors.
fn causes(error: &dyn std::error::Error) -> Vec<String> {
    let mut kept: Vec<String> = Vec::new();
    let mut next = error.source();

    while let Some(cause) = next {
        next = cause.source();
        let text = cause.to_string();
        if text.is_empty() || text.starts_with("client error (") {
            continue;
        }
        if kept.last().is_some_and(|last| last.contains(&text)) {
            continue;
        }
        kept.push(text);
    }

    kept
}

/// Why an exchange did not produce a response.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The HTTP client could not be created.
    #[error("cannot build the HTTP client: {0}")]
    Build(#[source] reqwest::Error),

    /// The CA bundle could not be read.
    #[error("cannot read the CA bundle `{path}`: {source}")]
    CaBundleRead {
        /// Bundle path.
        path: PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// The CA bundle is not valid PEM.
    #[error("cannot parse the CA bundle `{path}`: {source}")]
    CaBundleParse {
        /// Bundle path.
        path: PathBuf,
        /// Underlying TLS error.
        source: reqwest::Error,
    },

    /// The endpoint did not answer within the profile's timeout.
    #[error("the endpoint did not answer within {after_ms} ms")]
    Timeout {
        /// Timeout that elapsed.
        after_ms: u64,
    },

    /// Connection, TLS or protocol failure.
    ///
    /// The message is [`explain`]'s, not `reqwest`'s: the cause chain is the
    /// only part of a failed connection anybody can act on.
    #[error("{message}")]
    Send {
        /// What went wrong, causes included.
        message: String,
        /// The failure as `reqwest` reported it.
        #[source]
        source: Box<reqwest::Error>,
    },
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fmt;

    use super::*;

    /// A link in a chain that nests its cause's text, the way `hyper` does.
    #[derive(Debug)]
    struct Link {
        text: String,
        source: Option<Box<Link>>,
    }

    impl Link {
        fn new(text: &str, source: Option<Link>) -> Self {
            Self {
                text: text.to_owned(),
                source: source.map(Box::new),
            }
        }
    }

    impl fmt::Display for Link {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str(&self.text)
        }
    }

    impl Error for Link {
        fn source(&self) -> Option<&(dyn Error + 'static)> {
            self.source
                .as_deref()
                .map(|link| link as &(dyn Error + 'static))
        }
    }

    #[test]
    fn a_cause_the_one_above_already_said_is_not_said_twice() {
        // What a refused connection actually looks like: three links, one fact.
        let chain = Link::new(
            "error sending request",
            Some(Link::new(
                "client error (Connect)",
                Some(Link::new(
                    "tcp connect error: Connection refused (os error 61)",
                    Some(Link::new("Connection refused (os error 61)", None)),
                )),
            )),
        );

        assert_eq!(
            causes(&chain),
            vec!["tcp connect error: Connection refused (os error 61)"]
        );
    }

    #[test]
    fn a_name_that_does_not_resolve_keeps_the_sentence_that_says_so() {
        let chain = Link::new(
            "error sending request",
            Some(Link::new(
                "client error (Connect)",
                Some(Link::new(
                    "dns error: failed to lookup address information: nodename nor servname provided",
                    None,
                )),
            )),
        );

        let joined = causes(&chain).join(": ");
        assert!(
            joined.contains("failed to lookup address information"),
            "{joined}"
        );
        assert!(!joined.contains("client error"), "{joined}");
    }

    #[tokio::test]
    async fn a_refused_connection_says_more_than_the_url_did() {
        // Port 1 on the loopback: nothing listens there, and the refusal is
        // immediate, so this asserts on a real `reqwest` error without a server.
        let client = Client::new();
        let error = client
            .get("http://127.0.0.1:1/")
            .timeout(Duration::from_secs(5))
            .send()
            .await
            .expect_err("nothing listens on port 1");

        let message = explain(&error);
        assert!(
            message.starts_with("could not reach the endpoint"),
            "{message}"
        );
        // The point of the whole exercise: the reason, not just the address.
        assert!(message.contains("connect error"), "{message}");
        assert!(!message.contains("error sending request"), "{message}");
    }
}
