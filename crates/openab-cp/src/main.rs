//! Standalone control-plane binary.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{info, warn};

use openab_cp::config::CpConfig;
use openab_cp::server::{app, supervise_sweeper, AppState};

#[derive(Parser)]
#[command(name = "openab-cp", about = "OpenAB Agent Control Plane")]
struct Cli {
    /// Path to the CP config file (TOML).
    #[arg(short, long, default_value = "cp.toml")]
    config: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cli = Cli::parse();
    let cfg = CpConfig::load(&cli.config)?;
    let listen = cfg.listen.clone();
    if cfg.agents.is_empty() {
        tracing::warn!("no [[agents]] identities configured — every connection will be rejected");
    }
    info!(
        listen = %listen,
        identities = cfg.agents.len(),
        namespaces = cfg.namespaces.len(),
        "starting openab-cp"
    );

    let state = Arc::new(AppState::new(cfg));
    // The sweeper is supervised, not fire-and-forget: a panic (or a stall, or
    // an unexpected return) inside it would otherwise leave the CP answering
    // `/health` with `ok` while lease expiry and deadline sweeping are dead.
    // The supervisor restarts it with backoff and keeps `/health` honest.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let sweeper = tokio::spawn(supervise_sweeper(state.clone(), shutdown_rx));

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    axum::serve(listener, app(state))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
            info!("shutdown signal received");
        })
        .await?;

    // Signal the supervisor instead of aborting the sweeper handle: the abort
    // would bypass the supervision that was just installed. The supervisor
    // aborts the sweeper itself, on its way out.
    let _ = shutdown_tx.send(true);
    if tokio::time::timeout(Duration::from_secs(5), sweeper)
        .await
        .is_err()
    {
        // The handle is gone with the timeout, so this cannot be retried or
        // aborted; runtime teardown is the last resort. Say so rather than
        // exiting quietly with a sweeper of unknown state.
        warn!("sweeper supervisor did not stop within 5s of shutdown");
    }
    Ok(())
}
