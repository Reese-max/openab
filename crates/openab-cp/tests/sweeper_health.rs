//! Regression tests for issue #1474: the lease/deadline sweeper is the task
//! that makes `lease_expiry_secs` and delegation deadlines real. When it is
//! not running — never spawned, panicked, aborted, or stalled — the CP keeps
//! accepting registrations and delegations while maintenance silently stops.
//! `/health` must not answer `ok` in that state.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use openab_cp::config::CpConfig;
use openab_cp::server::{app, run_sweeper, AppState};

fn cfg() -> CpConfig {
    let cfg: CpConfig = toml::from_str(
        r#"
[[agents]]
key = "k"
namespace = "prod"
name = "koudu"
type = "primary"
"#,
    )
    .expect("test config parses");
    cfg.validate().expect("test config validates");
    cfg
}

/// Start a CP on an ephemeral loopback port; returns its state and address.
async fn spawn_cp() -> (Arc<AppState>, SocketAddr) {
    let state = Arc::new(AppState::new(cfg()));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    (state, addr)
}

/// `GET /health` over a raw socket — no HTTP client dependency.
async fn health_status(addr: SocketAddr) -> (u16, String) {
    let mut last_err = None;
    for _ in 0..100 {
        match TcpStream::connect(addr).await {
            Ok(mut s) => {
                s.write_all(
                    b"GET /health HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
                let mut buf = Vec::new();
                s.read_to_end(&mut buf).await.unwrap();
                let raw = String::from_utf8_lossy(&buf).to_string();
                let status = raw
                    .split_whitespace()
                    .nth(1)
                    .and_then(|t| t.parse::<u16>().ok())
                    .expect("a status line in the HTTP response");
                return (status, raw);
            }
            Err(e) => {
                // The listener is bound but the accept loop may not have
                // been scheduled yet.
                last_err = Some(e);
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
    panic!("never connected to /health: {last_err:?}");
}

/// Wait for `/health` to report `want` (or fail) within `within`.
async fn await_health(addr: SocketAddr, want: u16, within: Duration) -> (u16, String) {
    let deadline = Instant::now() + within;
    let mut last = (0u16, String::new());
    while Instant::now() < deadline {
        last = health_status(addr).await;
        if last.0 == want {
            return last;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    last
}

#[tokio::test]
async fn health_is_down_when_the_sweeper_never_ran() {
    // A CP serving without a sweeper must not claim ok: leases of
    // disconnected agents would never expire and deadlines never fire.
    let (_state, addr) = spawn_cp().await;
    let (status, raw) = health_status(addr).await;
    assert_eq!(
        status, 503,
        "/health must not report ok while no sweeper has ever run: {raw}"
    );
}

#[tokio::test]
async fn health_is_ok_while_the_sweeper_is_alive() {
    let (state, addr) = spawn_cp().await;
    let sweeper = tokio::spawn(run_sweeper(state));
    let (status, raw) = await_health(addr, 200, Duration::from_secs(5)).await;
    sweeper.abort();
    let _ = sweeper.await;
    assert_eq!(
        status, 200,
        "/health must report ok once the sweeper is running: {raw}"
    );
}

#[tokio::test]
async fn health_goes_down_when_the_sweeper_task_dies() {
    // The sweeper's task terminating — panic, return, or abort — must flip
    // /health immediately. On the unfixed base the JoinHandle is only
    // aborted at shutdown and the endpoint answers ok forever.
    let (state, addr) = spawn_cp().await;
    let sweeper = tokio::spawn(run_sweeper(state));
    let (status, raw) = await_health(addr, 200, Duration::from_secs(5)).await;
    assert_eq!(status, 200, "precondition: sweeper alive: {raw}");

    sweeper.abort();
    // Awaiting the handle is what the supervision in `main` does; by the
    // time it resolves, the task (and its teardown) is fully gone.
    let _ = sweeper.await;

    let (status, raw) = await_health(addr, 503, Duration::from_secs(5)).await;
    assert_eq!(
        status, 503,
        "a dead sweeper must take /health down, not keep answering ok: {raw}"
    );
}
