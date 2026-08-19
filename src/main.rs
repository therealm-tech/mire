//! Entry point: wire everything up, print where to go, serve until told to stop.

mod cli;
mod settings;

use std::net::SocketAddr;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use mire::api::{AppState, normalise_base_path, router};
use mire::config::{self, ConfigStore};
use mire::exec::Runner;
use mire::transport::{self, TransportOptions};
use mire::uploads::UploadStore;
use tracing::{debug, error, info, warn};
use tracing_subscriber::EnvFilter;

use crate::cli::Cli;
use crate::settings::{DEFAULT_LOG_FILTER, FileState, Settings};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    // Read before the subscriber exists, because `log_filter:` is one of the keys
    // the file may carry. Which is also why resolving it cannot log: this one
    // failure is reported just below, under the filter the file never got to
    // change.
    let settings = Settings::resolve(&cli);
    let log_filter = match &settings {
        Ok(settings) => settings.log_filter.clone(),
        Err(_) => cli
            .log_filter
            .clone()
            .unwrap_or_else(|| DEFAULT_LOG_FILTER.to_owned()),
    };

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(&log_filter))
        .with_writer(std::io::stderr)
        .init();

    let settings = match settings {
        Ok(settings) => settings,
        Err(error) => {
            error!(%error, "mire stopped");
            return ExitCode::FAILURE;
        }
    };

    match run(settings).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            error!(%error, "mire stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run(settings: Settings) -> Result<(), StartupError> {
    match &settings.file {
        FileState::Read(path) => info!(path = %path.display(), "configuration file read"),
        FileState::Absent(path) => debug!(path = %path.display(), "no configuration file"),
        FileState::Nowhere => debug!("no configuration file, and no home to look for one in"),
    }

    // Built first: the auth registry hands this client to its OIDC providers, so
    // their token exchanges trust the same CAs as every other outbound call.
    let client = transport::build_client(&TransportOptions {
        ca_bundle: settings.ca_bundle.clone(),
    })?;

    let config =
        ConfigStore::load(&settings.profiles, client.clone()).map_err(StartupError::Config)?;

    {
        let snapshot = config.snapshot();
        info!(
            profiles = snapshot.profiles.len(),
            providers = snapshot.registry.descriptors().len(),
            dirs = %config::describe(&settings.profiles),
            "configuration loaded"
        );
        // Never fatal: coming up and showing the problem beats refusing to start.
        for issue in snapshot.issues() {
            warn!(%issue, "configuration issue");
        }
    }

    // Kept alive for the lifetime of the process: dropping it stops the watch.
    let _watcher = config::watch(Arc::clone(&config))?;

    let base_path = normalise_base_path(&settings.base_path);
    let state = AppState {
        runner: Runner::new(config, client),
        uploads: Arc::new(UploadStore::new(&settings.uploads)),
        base_path: base_path.clone().into(),
        public_url: settings.public_url.clone().map(Into::into),
    };

    let address = SocketAddr::new(settings.host, settings.port);
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
    // The path is inside the error: with a list of directories, "no such file or
    // directory" on its own does not say which one you got wrong.
    #[error("cannot read the configuration directory {0}")]
    Config(#[source] std::io::Error),

    #[error("cannot watch the configuration directories: {0}")]
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
