//! Standalone control-plane binary.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use tracing::{info, warn};

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
    let drain_budget = Duration::from_secs(state.cfg.shutdown_drain_secs);
    // The process's exit is bounded by the drain budget plus slack for the
    // listener's own bookkeeping. One knob, one bound: an orchestrator
    // escalates to SIGKILL at the end of its grace period, so the CP's own
    // deadline has to be the shorter one.
    let hard_bound = drain_budget + Duration::from_secs(1);
    let sweeper = tokio::spawn(run_sweeper(state.clone()));

    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .with_context(|| format!("binding {listen}"))?;

    // The shutdown sequence is SPAWNED rather than awaited after the listener.
    // Ordering it after `serve` was a real defect: axum's graceful shutdown
    // ends when the last in-flight HTTP request finishes, and a peer that
    // opened a socket and then said nothing (a half-sent request, a stalled
    // upgrade) pins that wait for as long as the orchestrator allows the
    // process to live — so the drain would never start, and the CP would be
    // SIGKILLed with its delegations unresolved. Spawning it makes the drain
    // begin the instant the signal lands, whatever hyper is doing.
    let (stop_accepting_tx, stop_accepting_rx) = tokio::sync::oneshot::channel::<()>();
    let (drained_tx, drained_rx) = tokio::sync::oneshot::channel::<()>();
    tokio::spawn({
        let state = Arc::clone(&state);
        async move {
            let signal = shutdown_signal().await;
            info!(signal, "shutdown signal received — draining connections");
            // A second signal at ANY point before the process exits is the
            // operator's "stop waiting": exit immediately with the signal's
            // conventional status (128 + signum). Armed before the drain and
            // deliberately left armed — the drain and the listener's wait are
            // separate waits, and the escape hatch has to cover both. The task
            // ends with the process if it never fires.
            tokio::spawn(async {
                let signal = shutdown_signal().await;
                warn!(signal, "second shutdown signal — forcing immediate exit");
                std::process::exit(signal_exit_code(signal));
            });
            // Nothing new may enter a CP that is leaving.
            let _ = stop_accepting_tx.send(());
            graceful_shutdown(&state).await;
            let _ = drained_tx.send(());
        }
    });

    // The listener stops accepting as soon as the signal lands, and the wait
    // for it is bounded by the same hard bound — hyper's bookkeeping must not
    // be able to outlive the drain it is supposed to follow.
    let serving =
        axum::serve(listener, app(Arc::clone(&state))).with_graceful_shutdown(async move {
            let _ = stop_accepting_rx.await;
        });
    let finished = tokio::time::timeout(hard_bound, async {
        tokio::join!(
            async {
                // The drain is the shutdown: the synthesized terminals and the
                // close frames get their chance to reach the wire here.
                let _ = drained_rx.await;
            },
            async {
                match tokio::time::timeout(hard_bound, serving).await {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => warn!(error = %e, "listener stopped with an error"),
                    Err(_) => warn!(
                        bound_secs = hard_bound.as_secs(),
                        "the listener's graceful wait outlived the shutdown bound — dropping it"
                    ),
                }
            },
        );
    })
    .await;
    if finished.is_err() {
        warn!(
            bound_secs = hard_bound.as_secs(),
            "shutdown did not finish within its bound — exiting with the drain unfinished"
        );
    }

    // Only now: the sweeper exists to keep registrations and delegations
    // honest, and during a shutdown its findings would race the closes above.
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
