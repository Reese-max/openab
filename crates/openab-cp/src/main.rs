//! Standalone control-plane binary.

use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::info;

use openab_cp::config::CpConfig;
use openab_cp::server::{app, graceful_shutdown, run_sweeper, AppState};

#[derive(Parser)]
#[command(name = "openab-cp", about = "OpenAB Agent Control Plane")]
struct Cli {
    /// Path to the CP config file (TOML).
    #[arg(short, long, default_value = "cp.toml")]
    config: String,
}

/// Wait for a shutdown signal: SIGINT (ctrl-c) on every platform, plus
/// SIGTERM and SIGHUP on unix — the signals `docker stop`, ECS and k8s send
/// before escalating to SIGKILL. Without handlers the default disposition
/// kills the process outright: no close frames, no terminal results, and
/// runtimes see TCP resets.
async fn shutdown_signal() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = match signal(SignalKind::terminate()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "failed to install SIGTERM handler — falling back to ctrl-c only"
                );
                let _ = tokio::signal::ctrl_c().await;
                return "SIGINT";
            }
        };
        let mut hup = match signal(SignalKind::hangup()) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "failed to install SIGHUP handler");
                return tokio::select! {
                    _ = tokio::signal::ctrl_c() => "SIGINT",
                    _ = term.recv() => "SIGTERM",
                };
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => "SIGINT",
            _ = term.recv() => "SIGTERM",
            _ = hup.recv() => "SIGHUP",
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        "SIGINT"
    }
}

/// Conventional exit status for a signal-driven exit: 128 + signal number.
fn signal_exit_code(signal: &str) -> i32 {
    match signal {
        "SIGHUP" => 129,
        "SIGINT" => 130,
        "SIGTERM" => 143,
        _ => 1,
    }
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
    let sweeper = tokio::spawn(run_sweeper(state.clone()));

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;
    axum::serve(listener, app(state.clone()))
        .with_graceful_shutdown(async {
            let signal = shutdown_signal().await;
            info!(signal, "shutdown signal received — draining connections");
        })
        .await?;

    // A second signal while the drain is still running is the operator's
    // "stop waiting": exit immediately with the signal's conventional
    // status. `shutdown_signal` registers fresh streams each call, so the
    // repeat SIGINT/SIGTERM/SIGHUP still resolves it — the drain itself is
    // bounded, so this only shortens a wait, never causes one.
    let force_quit = tokio::spawn(async {
        let signal = shutdown_signal().await;
        tracing::warn!(signal, "second shutdown signal — forcing immediate exit");
        std::process::exit(signal_exit_code(signal));
    });

    // The listener has stopped accepting and in-flight HTTP work is done;
    // the WS connection tasks are detached spawns still running. Give them a
    // real shutdown — synthesized terminals, close frames, bounded drain —
    // instead of letting process exit reset their sockets. The sweeper
    // stops last: a lease/deadline expiry during the drain still
    // synthesizes normally.
    graceful_shutdown(&state).await;
    force_quit.abort();
    sweeper.abort();
    let _ = sweeper.await;
    info!("openab-cp stopped");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::signal_exit_code;

    #[test]
    fn signal_exit_codes_follow_the_128_plus_signum_convention() {
        // The force-quit path must report WHICH signal ended the process so
        // an operator (or a supervisor's exit-code log) can tell a forced
        // exit from a clean drain — and from a crash.
        assert_eq!(signal_exit_code("SIGHUP"), 129);
        assert_eq!(signal_exit_code("SIGINT"), 130);
        assert_eq!(signal_exit_code("SIGTERM"), 143);
    }
}
