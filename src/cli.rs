//! Command line. One binary, no subcommand: `mire` serves the API and the UI.

use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use clap::Parser;

/// A test pattern for model endpoints.
#[derive(Debug, Parser)]
#[command(name = "mire", version, about, long_about = None)]
pub struct Cli {
    /// Directory holding the profile YAML files (and, optionally, `auth.yaml`).
    #[arg(long, env = "PROFILES_DIR", default_value = "./profiles")]
    pub profiles: PathBuf,

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
