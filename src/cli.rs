//! Command line. One binary, no subcommand: `mire` serves the API and the UI.
//!
//! Every option is three things — a flag, an environment variable, and a key in
//! the configuration file — and they win in that order. Which is why nothing here
//! carries a `default_value`: a default would be indistinguishable from something
//! you asked for, and would quietly outrank the file. The defaults live in
//! [`crate::settings`], where the layering happens; the help text names them so
//! `--help` still tells the whole story.

use std::net::IpAddr;
use std::path::PathBuf;

use clap::Parser;

/// A test pattern for model endpoints.
#[derive(Debug, Parser)]
#[command(name = "mire", version, about, long_about = None)]
pub struct Cli {
    /// Configuration file to read [default: `~/.config/mire/mire.yaml`].
    ///
    /// Holds the same options as the flags below, in YAML, under the same names
    /// with underscores (`base_path`, `log_filter`). Anything a flag or an
    /// environment variable also says wins over it.
    ///
    /// The default location is optional and simply not read when it is not there.
    /// A file named here is not: if `mire` cannot read it, it stops and says so,
    /// because a typo in this path must not look like having no configuration.
    #[arg(long, env = "CONFIG_FILE")]
    pub config: Option<PathBuf>,

    /// Directory holding the profile YAML files (and, optionally, `auth.yaml`)
    /// [default: `./profiles`].
    ///
    /// Repeatable, and `:`-separated in the environment variable, the way `PATH`
    /// is. Several directories are layered in the order given: a profile — or an
    /// auth provider, MCP server, saved prompt — declared in more than one
    /// belongs to the last directory that declares it, and the one it displaced
    /// is named in a warning. A directory somebody else maintains, and yours on
    /// top of it, without copying theirs to change one line.
    #[arg(long, env = "PROFILES_DIR", value_delimiter = ':')]
    pub profiles: Option<Vec<PathBuf>>,

    /// Directory attached files are written to [default: `./uploads`].
    ///
    /// Created on the first upload, not at startup: `mire` otherwise writes
    /// nothing at all, and a read-only filesystem should only be a problem for
    /// somebody who actually attaches something.
    #[arg(long, env = "UPLOADS_DIR")]
    pub uploads: Option<PathBuf>,

    /// Address to listen on [default: `127.0.0.1`].
    ///
    /// Localhost by default; widening it is a deliberate act.
    #[arg(long, env = "HOST")]
    pub host: Option<IpAddr>,

    /// Port to listen on [default: `8787`].
    #[arg(long, env = "PORT")]
    pub port: Option<u16>,

    /// Path prefix for every route, for a proxied notebook
    /// (e.g. `/notebook/<namespace>/<name>/proxy/8787`).
    #[arg(long, env = "BASE_PATH")]
    pub base_path: Option<String>,

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

    /// `tracing` filter directive [default: `info`].
    ///
    /// Syntax: <https://docs.rs/tracing-subscriber/latest/tracing_subscriber/filter/struct.EnvFilter.html#directives>
    /// For example `info`, or `mire=debug,tower_http=debug`.
    #[arg(long = "log-filter", env = "LOG_FILTER")]
    pub log_filter: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        Cli::try_parse_from(std::iter::once("mire").chain(args.iter().copied())).unwrap()
    }

    /// Absent means absent, all the way down: the defaults are
    /// [`crate::settings`]'s business, and a `default_value` here would outrank
    /// the configuration file.
    #[test]
    fn an_option_nobody_passed_stays_unset() {
        let cli = parse(&[]);

        assert_eq!(cli.config, None);
        assert_eq!(cli.profiles, None);
        assert_eq!(cli.uploads, None);
        assert_eq!(cli.host, None);
        assert_eq!(cli.port, None);
        assert_eq!(cli.base_path, None);
        assert_eq!(cli.log_filter, None);
    }

    #[test]
    fn one_directory_is_still_one_directory() {
        assert_eq!(
            parse(&["--profiles", "./profiles"]).profiles,
            Some(vec![PathBuf::from("./profiles")])
        );
    }

    #[test]
    fn the_flag_repeats_and_keeps_the_order_it_was_given() {
        assert_eq!(
            parse(&["--profiles", "./base", "--profiles", "./mine"]).profiles,
            Some(vec![PathBuf::from("./base"), PathBuf::from("./mine")])
        );
    }

    /// The same separator `PATH` uses, so that `PROFILES_DIR` — which is one
    /// string and cannot be repeated — can carry a list at all.
    #[test]
    fn a_colon_separated_value_is_a_list() {
        assert_eq!(
            parse(&["--profiles", "./base:./mine"]).profiles,
            Some(vec![PathBuf::from("./base"), PathBuf::from("./mine")])
        );
    }

    #[test]
    fn the_configuration_file_can_be_named() {
        assert_eq!(
            parse(&["--config", "/etc/mire/mire.yaml"]).config,
            Some(PathBuf::from("/etc/mire/mire.yaml"))
        );
    }
}
