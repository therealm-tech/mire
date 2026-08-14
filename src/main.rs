//! Entry point: wire everything up, print where to go, serve until told to stop.

mod cli;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use mire::api::{AppState, normalise_base_path, router};
use mire::config::{self, ConfigStore};
use mire::exec::Runner;
use mire::transport::{self, TransportOptions};
use mire::uploads::UploadStore;
use tracing::{error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::cli::Cli;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&cli.log_filter))
        .with_writer(std::io::stderr)
        .init();

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "mire stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), StartupError> {
    // Built first: the auth registry hands this client to its OIDC providers, so
    // their token exchanges trust the same CAs as every other outbound call.
    let client = transport::build_client(&TransportOptions {
        ca_bundle: cli.ca_bundle.clone(),
    })?;

    let config = ConfigStore::load(&cli.profiles, client.clone()).map_err(|source| {
        StartupError::Config {
            path: cli.profiles.display().to_string(),
            source,
        }
    })?;

    {
        let snapshot = config.snapshot();
        info!(
            profiles = snapshot.profiles.len(),
            providers = snapshot.registry.descriptors().len(),
            dir = %cli.profiles.display(),
            "configuration loaded"
        );
        // Never fatal: coming up and showing the problem beats refusing to start.
        for issue in snapshot.issues() {
            warn!(%issue, "configuration issue");
        }
    }

    // Kept alive for the lifetime of the process: dropping it stops the watch.
    let _watcher = config::watch(Arc::clone(&config))?;

    let base_path = normalise_base_path(&cli.base_path);
    let state = AppState {
        runner: Runner::new(config, client),
        uploads: Arc::new(UploadStore::new(&cli.uploads)),
        base_path: base_path.clone().into(),
        public_url: cli.public_url.clone().map(Into::into),
    };

    let address = SocketAddr::new(cli.host, cli.port);
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .map_err(|source| StartupError::Bind { address, source })?;

    let bound = listener
        .local_addr()
        .map_err(|source| StartupError::Bind { address, source })?;
    info!(url = %format!("http://{bound}{base_path}/"), "mire is up");
    info!(url = %format!("http://{bound}{base_path}/docs"), "API reference");

    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(StartupError::Serve)?;

    info!("mire stopped");
    Ok(())
}

/// Completes on `SIGTERM` or `SIGINT`.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let mut interrupt = match signal(SignalKind::interrupt()) {
        Ok(stream) => stream,
        Err(error) => {
            error!(%error, "cannot install the SIGINT handler");
            return;
        }
    };
    let mut terminate = match signal(SignalKind::terminate()) {
        Ok(stream) => stream,
        Err(error) => {
            error!(%error, "cannot install the SIGTERM handler");
            return;
        }
    };

    tokio::select! {
        _ = interrupt.recv() => info!("SIGINT received, draining"),
        _ = terminate.recv() => info!("SIGTERM received, draining"),
    }
}

/// Why `mire` could not start.
#[derive(Debug, thiserror::Error)]
enum StartupError {
    #[error("cannot read the configuration directory `{path}`: {source}")]
    Config {
        path: String,
        source: std::io::Error,
    },

    #[error("cannot watch the configuration directory: {0}")]
    Watch(#[from] notify::Error),

    #[error(transparent)]
    Transport(#[from] mire::transport::TransportError),

    #[error("cannot bind {address}: {source}")]
    Bind {
        address: SocketAddr,
        source: std::io::Error,
    },

    #[error("the server stopped unexpectedly: {0}")]
    Serve(#[source] std::io::Error),
}
