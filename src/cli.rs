//! Command line. One binary, no subcommand: `mire` serves the API and the UI.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use clap::Parser;

/// A test pattern for model endpoints.
#[derive(Debug, Parser)]
#[command(name = "mire", version, about, long_about = None)]
pub struct Cli {
    /// Directory holding the profile YAML files (and, optionally, `auth.yaml`).
    ///
    /// Repeatable, and `:`-separated in the environment variable, the way `PATH`
    /// is. Several directories are layered in the order given: a profile — or an
    /// auth provider, MCP server, saved prompt — declared in more than one
    /// belongs to the last directory that declares it, and the one it displaced
    /// is named in a warning. A directory somebody else maintains, and yours on
    /// top of it, without copying theirs to change one line.
    #[arg(
        long,
        env = "PROFILES_DIR",
        value_delimiter = ':',
        default_value = "./profiles"
    )]
    pub profiles: Vec<PathBuf>,

    /// Directory attached files are written to.
    ///
    /// Created on the first upload, not at startup: `mire` otherwise writes
    /// nothing at all, and a read-only filesystem should only be a problem for
    /// somebody who actually attaches something.
    #[arg(long, env = "UPLOADS_DIR", default_value = "./uploads")]
    pub uploads: PathBuf,

    /// Address to listen on. Localhost by default; widening it is a deliberate act.
    #[arg(long, env = "HOST", default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
    pub host: IpAddr,

    /// Port to listen on.
    #[arg(long, env = "PORT", default_value_t = 8787)]
    pub port: u16,

    /// Path prefix for every route, for a proxied notebook
    /// (e.g. `/notebook/<namespace>/<name>/proxy/8787`).
    #[arg(long, env = "BASE_PATH", default_value = "")]
    pub base_path: String,

    /// Origin the browser reaches `mire` on, e.g. `https://kubeflow.example`.
    ///
    /// Only the OIDC browser login uses it, to build its callback URL. Leave it
    /// unset and the UI supplies the origin it is actually being served from,
    /// which is right in every case that does not involve a proxy rewriting
    /// paths. Set it when it is wrong; the value must match what the identity
    /// provider has registered.
    #[arg(long, env = "PUBLIC_URL")]
    pub public_url: Option<String>,

    /// PEM bundle of extra certificate authorities to trust.
    #[arg(long, env = "CA_BUNDLE")]
    pub ca_bundle: Option<PathBuf>,

    /// `tracing` filter directive.
    ///
    /// Syntax: <https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives>
    /// For example `info`, or `mire=debug,tower_http=debug`.
    #[arg(long = "log-filter", env = "LOG_FILTER", default_value = "info")]
    pub log_filter: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("mire").chain(args.iter().copied())).unwrap()
    }

    #[test]
    fn one_directory_is_still_the_default() {
        assert_eq!(parse(&[]).profiles, [PathBuf::from("./profiles")]);
    }

    #[test]
    fn the_flag_repeats_and_keeps_the_order_it_was_given() {
        assert_eq!(
            parse(&["--profiles", "./base", "--profiles", "./mine"]).profiles,
            [PathBuf::from("./base"), PathBuf::from("./mine")]
        );
    }

    /// The same separator `PATH` uses, so that `PROFILES_DIR` — which is one
    /// string and cannot be repeated — can carry a list at all.
    #[test]
    fn a_colon_separated_value_is_a_list() {
        assert_eq!(
            parse(&["--profiles", "./base:./mine"]).profiles,
            [PathBuf::from("./base"), PathBuf::from("./mine")]
        );
    }

    /// A default is not a layer: naming one directory must not silently keep
    /// `./profiles` underneath it.
    #[test]
    fn naming_a_directory_replaces_the_default_rather_than_layering_on_it() {
        assert_eq!(
            parse(&["--profiles", "./mine"]).profiles,
            [PathBuf::from("./mine")]
        );
    }
}
