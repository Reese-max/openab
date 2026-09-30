//! Standalone control-plane binary.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;

use openab_cp::config::CpConfig;
use openab_cp::server::{app, run_sweeper, AppState};

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
    let mut sweeper = tokio::spawn(run_sweeper(state.clone()));

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    let server = axum::serve(listener, app(state)).with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
        info!("shutdown signal received");
    });

    // Poll the sweeper's handle, never park it: the task is an infinite
    // loop, so its JoinHandle resolving — panic, return, anything — means
    // lease expiry and deadline sweeping are dead while the CP keeps
    // accepting work (#1474). There is no in-process recovery a restarted
    // loop would not immediately re-trip, and the task's exit guard has
    // already turned /health over — so the outcome is fatal: exit non-zero
    // and let the process supervisor restart a clean CP.
    tokio::select! {
        res = server => {
            res?;
            sweeper.abort();
            Ok(())
        }
        res = &mut sweeper => {
            match &res {
                Ok(()) => tracing::error!(
                    "sweeper task returned — it is an infinite loop, so this is a bug"
                ),
                Err(e) if e.is_panic() => tracing::error!(
                    error = %e,
                    "sweeper task panicked — lease expiry and deadline sweeping are dead"
                ),
                // Only reachable if the task is cancelled from elsewhere;
                // the abort below runs after this select resolves.
                Err(e) => tracing::error!(error = %e, "sweeper task terminated abnormally"),
            }
            anyhow::bail!(
                "sweeper task terminated — exiting so the supervisor restarts a clean CP"
            )
        }
    }
}
