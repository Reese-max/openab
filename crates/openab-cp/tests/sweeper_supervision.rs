//! Supervision and liveness of the CP's background sweeper (#1474).
//!
//! A sweeper that dies silently turns lease expiry and deadline sweeping into
//! a no-op while the process keeps serving: leases of disconnected agents never
//! expire, deadline-overdue delegations never resolve, and nothing above the
//! CP can tell — `/health` still answers `ok`. These tests pin the two
//! properties that close that gap:
//!
//! 1. a dead sweeper is *supervised* — restarted with backoff, whether it
//!    panicked, returned, or stalled without completing a pass; and
//! 2. `/health` reports `503` for exactly as long as sweeping is known to be
//!    stopped, and recovers on the first pass of the replacement.
//!
//! Every supervision test drives the real supervisor loop on a paused clock
//! with a compressed policy, so the timings are simulated and deterministic.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio_tungstenite::tungstenite::http::StatusCode;

use openab_cp::config::CpConfig;
use openab_cp::server::{app, supervise_sweeper_task, AppState, SweeperStatus, SweeperSupervision};

/// Compressed supervision policy: the production values (1 s watchdog poll,
/// 30 s stall window, 250 ms → 30 s backoff) would make these tests either
/// minutes long or dependent on wall-clock slack. The *shape* under test is the
/// same; only the scale differs.
fn fast_policy() -> SweeperSupervision {
    SweeperSupervision {
        poll_interval: Duration::from_millis(10),
        stall_timeout: Duration::from_millis(200),
        initial_backoff: Duration::from_millis(10),
        max_backoff: Duration::from_millis(100),
    }
}

/// Restart ceiling for the backoff-interruption test: long enough that a
/// shutdown which waits its delay out cannot possibly fit in the test's timeout.
const CEILING: Duration = Duration::from_secs(300);

/// CP state with the stock (all-default) configuration.
fn state() -> Arc<AppState> {
    let cfg: CpConfig = toml::from_str("").expect("default config parses");
    cfg.validate().expect("default config validates");
    Arc::new(AppState::new(cfg))
}

/// Wait until `done` holds, on the (simulated) clock.
async fn wait_until(label: &str, mut done: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(60), async {
        while !done() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {label}"));
}

#[tokio::test]
async fn health_is_ok_only_while_the_sweeper_is_alive() {
    // The reported failure mode is "the process stays up and /health still
    // returns ok" after the sweeper dies. A CP that answers 200 while it knows
    // sweeping has stopped is lying to its orchestrator, so the death has to
    // move the endpoint to 503 and the first pass of the replacement has to
    // move it back.
    let state = state();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = app(state.clone());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let (status, body) = http_get(addr, "/health").await;
    assert_eq!(status, StatusCode::OK, "a live sweeper must report ok");
    assert_eq!(body, "ok");

    state.record_sweeper_death();
    let (status, body) = http_get(addr, "/health").await;
    assert_eq!(
        status,
        StatusCode::SERVICE_UNAVAILABLE,
        "a dead sweeper must be visible to the orchestrator, not hidden behind a 200"
    );
    assert!(
        body.contains("sweeper"),
        "the 503 body must name the failed component, got {body:?}"
    );

    state.record_sweeper_pass();
    let (status, body) = http_get(addr, "/health").await;
    assert_eq!(
        status,
        StatusCode::OK,
        "the first pass of a replacement sweeper must clear the degraded state"
    );
    assert_eq!(body, "ok");
}

#[tokio::test(start_paused = true)]
async fn a_panicking_sweeper_is_restarted_and_the_replacement_comes_up_degraded() {
    // A panic inside the sweeper used to end the task with nothing watching:
    // the CP kept accepting registrations and delegations while lease expiry
    // and deadlines silently stopped. The supervisor must bring a replacement
    // up — and must not let that replacement start while `/health` still
    // claims sweeping is fine.
    let state = state();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let starts = Arc::new(AtomicUsize::new(0));
    let seen: Arc<Mutex<Vec<SweeperStatus>>> = Arc::new(Mutex::new(Vec::new()));

    let starts_task = Arc::clone(&starts);
    let seen_task = Arc::clone(&seen);
    let state_task = Arc::clone(&state);
    let supervisor = tokio::spawn(supervise_sweeper_task(
        state.clone(),
        shutdown_rx,
        fast_policy(),
        move || {
            let starts = Arc::clone(&starts_task);
            let seen = Arc::clone(&seen_task);
            let state = Arc::clone(&state_task);
            async move {
                // Sampled at spawn time: what `/health` would have said the
                // instant this sweeper existed.
                seen.lock().push(state.sweeper_status());
                let nth = starts.fetch_add(1, Ordering::SeqCst);
                if nth == 0 {
                    panic!("injected sweeper panic");
                }
                // A healthy replacement keeps making progress.
                loop {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    state.record_sweeper_pass();
                }
            }
        },
    ));

    wait_until("the replacement sweeper to be running and healthy", || {
        starts.load(Ordering::SeqCst) >= 2 && !state.sweeper_status().degraded
    })
    .await;

    let status = state.sweeper_status();
    assert!(status.deaths >= 1, "the panic must be recorded as a death");
    assert!(
        status.passes >= 1,
        "the replacement must actually sweep (pass counter advances)"
    );

    let seen = seen.lock().clone();
    assert!(
        !seen[0].degraded,
        "a CP that never lost its sweeper is not degraded"
    );
    assert!(
        seen[1].degraded,
        "a replacement must not come up while health still reports ok — \
         that window is exactly when sweeping is stopped and nothing says so"
    );

    drop(shutdown_tx);
    supervisor
        .await
        .expect("the supervisor task itself must not panic");
}

#[tokio::test(start_paused = true)]
async fn a_sweeper_that_stops_making_progress_is_restarted() {
    // Panic supervision alone is not enough: a task can wedge *inside* a pass
    // (a park it never leaves) and never return, so no `JoinHandle` ever
    // resolves. The pass counter is the only liveness evidence available, so a
    // window with no completed pass has to be treated as death.
    let state = state();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let starts = Arc::new(AtomicUsize::new(0));
    let seen: Arc<Mutex<Vec<SweeperStatus>>> = Arc::new(Mutex::new(Vec::new()));

    let starts_task = Arc::clone(&starts);
    let seen_task = Arc::clone(&seen);
    let state_task = Arc::clone(&state);
    let supervisor = tokio::spawn(supervise_sweeper_task(
        state.clone(),
        shutdown_rx,
        fast_policy(),
        move || {
            let starts = Arc::clone(&starts_task);
            let seen = Arc::clone(&seen_task);
            let state = Arc::clone(&state_task);
            async move {
                seen.lock().push(state.sweeper_status());
                starts.fetch_add(1, Ordering::SeqCst);
                // Never returns and never records a pass.
                std::future::pending::<()>().await;
            }
        },
    ));

    wait_until("the stalled sweeper to be replaced twice", || {
        starts.load(Ordering::SeqCst) >= 3
    })
    .await;

    let seen = seen.lock().clone();
    assert!(
        seen[1].degraded && seen[2].degraded,
        "every replacement of a stalled sweeper must come up degraded, got {seen:?}"
    );
    assert!(
        state.sweeper_status().deaths >= 2,
        "each stall must be recorded, got {:?}",
        state.sweeper_status()
    );

    drop(shutdown_tx);
    supervisor
        .await
        .expect("the supervisor task itself must not panic");
}

#[tokio::test(start_paused = true)]
async fn a_crash_looping_sweeper_is_backed_off_not_hot_restarted() {
    // Restarting unconditionally would turn a deterministic panic into a busy
    // loop that starves the runtime — a second silent failure with a much
    // worse symptom. Restarts are rate-limited by a backoff that doubles up to
    // a ceiling, so a crash loop costs a bounded number of attempts per second.
    let state = state();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let starts = Arc::new(AtomicUsize::new(0));
    let starts_task = Arc::clone(&starts);
    let supervisor = tokio::spawn(supervise_sweeper_task(
        state.clone(),
        shutdown_rx,
        fast_policy(),
        move || {
            let starts = Arc::clone(&starts_task);
            async move {
                starts.fetch_add(1, Ordering::SeqCst);
                panic!("injected sweeper panic");
            }
        },
    ));

    // Two simulated seconds against a 100 ms ceiling: a hot restart loop would
    // produce thousands of attempts here.
    tokio::time::sleep(Duration::from_secs(2)).await;
    let attempts = starts.load(Ordering::SeqCst);
    assert!(
        (2..=40).contains(&attempts),
        "a crash-looping sweeper must be retried with backoff, not in a hot loop \
         ({attempts} attempts in 2 simulated seconds)"
    );

    drop(shutdown_tx);
    supervisor
        .await
        .expect("the supervisor task itself must not panic");
}

#[tokio::test]
async fn a_long_lived_sweeper_resets_the_backoff_budget() {
    // Backoff must not be a permanent penalty: a sweeper that died after hours
    // of healthy service is a fresh incident, and its replacement must be
    // allowed to start promptly instead of inheriting the ceiling from an old
    // crash loop.
    let policy = fast_policy();
    assert_eq!(
        policy.backoff_after_consecutive_deaths(0),
        policy.initial_backoff
    );
    assert_eq!(
        policy.backoff_after_consecutive_deaths(1),
        policy.initial_backoff * 2,
    );
    assert!(
        policy.backoff_after_consecutive_deaths(2) > policy.backoff_after_consecutive_deaths(1),
        "repeated deaths must grow the delay"
    );
    assert_eq!(
        policy.backoff_after_consecutive_deaths(64),
        policy.max_backoff,
        "the delay is clamped, and a huge death count must not overflow"
    );
}

#[tokio::test(start_paused = true)]
async fn a_long_healthy_run_resets_the_delay_seen_by_the_next_death() {
    // The delay a replacement waits is only meaningful if it tracks the CURRENT
    // incident. A sweeper that ran healthily for longer than the stall window
    // and then died is a fresh incident: its replacement must get the initial
    // backoff, not a doubled one inherited from history. End to end this shows
    // up as an exact restart delay on the simulated clock.
    let policy = fast_policy();
    let state = state();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let starts = Arc::new(AtomicUsize::new(0));
    let start_times: Arc<Mutex<Vec<tokio::time::Instant>>> = Arc::new(Mutex::new(Vec::new()));

    let starts_task = Arc::clone(&starts);
    let times_task = Arc::clone(&start_times);
    let state_task = Arc::clone(&state);
    let supervisor = tokio::spawn(supervise_sweeper_task(
        state.clone(),
        shutdown_rx,
        policy,
        move || {
            let starts = Arc::clone(&starts_task);
            let times = Arc::clone(&times_task);
            let state = Arc::clone(&state_task);
            async move {
                times.lock().push(tokio::time::Instant::now());
                let nth = starts.fetch_add(1, Ordering::SeqCst);
                if nth == 0 {
                    // A long, healthy run: three stall windows of passes, then
                    // it dies.
                    for _ in 0..30 {
                        tokio::time::sleep(Duration::from_millis(20)).await;
                        state.record_sweeper_pass();
                    }
                }
                panic!("injected sweeper panic");
            }
        },
    ));

    wait_until("three sweeper generations", || {
        starts.load(Ordering::SeqCst) >= 3
    })
    .await;
    let times = start_times.lock().clone();

    assert_eq!(
        times[2] - times[1],
        Duration::from_millis(20),
        "the first death after a long healthy run starts a fresh budget, so its \
         replacement waits 2x the initial backoff ({:?} -> {:?}); a budget carried \
         over from the previous incident would make this delay different",
        policy.initial_backoff,
        policy.initial_backoff * 2,
    );

    drop(shutdown_tx);
    supervisor
        .await
        .expect("the supervisor task itself must not panic");
}

#[tokio::test(start_paused = true)]
async fn shutdown_while_the_sweeper_runs_terminates_it_and_waits() {
    // `abort()` only *requests* cancellation, and a dropped `JoinHandle`
    // detaches: a supervisor that returned on the abort alone would leave the
    // sweeper's cancellation uncarrried out while the rest of the process tears
    // down. The sweeper must be gone by the time the supervisor returns.
    let state = state();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let dropped = Arc::new(AtomicBool::new(false));

    struct DropMarksCancellation(Arc<AtomicBool>);
    impl Drop for DropMarksCancellation {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    let dropped_task = Arc::clone(&dropped);
    let supervisor = tokio::spawn(supervise_sweeper_task(
        state.clone(),
        shutdown_rx,
        fast_policy(),
        move || {
            let dropped = Arc::clone(&dropped_task);
            async move {
                let _marks_cancellation = DropMarksCancellation(dropped);
                std::future::pending::<()>().await;
            }
        },
    ));

    // Let the sweeper actually start before asking the supervisor to stop.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        !dropped.load(Ordering::SeqCst),
        "the sweeper must still be running before shutdown"
    );
    shutdown_tx.send(true).expect("supervisor holds a receiver");

    // Snapshot the flag in the same poll that observes the supervisor's
    // return. Reading it from this task instead would let the scheduler run the
    // aborted sweeper before the assertion, which is exactly what makes the
    // un-awaited variant look correct.
    let observed = Arc::new(AtomicBool::new(false));
    let observer = {
        let observed = Arc::clone(&observed);
        tokio::spawn(async move {
            let outcome = tokio::time::timeout(Duration::from_secs(5), supervisor).await;
            observed.store(dropped.load(Ordering::SeqCst), Ordering::SeqCst);
            outcome
        })
    };
    observer
        .await
        .expect("the observer must not panic")
        .expect("the supervisor must return on shutdown, not hang")
        .expect("the supervisor task itself must not panic");
    assert!(
        observed.load(Ordering::SeqCst),
        "the sweeper must be torn down before the supervisor returns — a dropped \
         JoinHandle detaches, so the abort is not carried out until later"
    );
}

#[tokio::test(start_paused = true)]
async fn shutdown_interrupts_the_backoff_wait() {
    // A crash-looping sweeper sits in the backoff between restarts; shutdown
    // must not have to wait the delay out (which may have grown to the
    // ceiling), or a process asked to stop could hang on the way out.
    let state = state();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let starts = Arc::new(AtomicUsize::new(0));

    let starts_task = Arc::clone(&starts);
    let supervisor = tokio::spawn(supervise_sweeper_task(
        state.clone(),
        shutdown_rx,
        SweeperSupervision {
            max_backoff: CEILING,
            ..fast_policy()
        },
        move || {
            let starts = Arc::clone(&starts_task);
            async move {
                starts.fetch_add(1, Ordering::SeqCst);
                panic!("injected sweeper panic");
            }
        },
    ));

    // Grow the delay to its ceiling and let most of it elapse: a generation
    // panics instantly, so each retry doubles the wait (20/40/80/... ms) until
    // it is clamped at CEILING. After this sleep the supervisor is parked in a
    // full CEILING-long wait with ~50 s still to run, which an uninterruptible
    // shutdown cannot fit inside the 5 s timeout below.
    tokio::time::sleep(CEILING - Duration::from_secs(50)).await;
    let started_at = tokio::time::Instant::now();
    shutdown_tx.send(true).expect("supervisor holds a receiver");

    tokio::time::timeout(Duration::from_secs(5), supervisor)
        .await
        .expect("shutdown must interrupt the backoff wait, not sit out its ceiling")
        .expect("the supervisor task itself must not panic");
    assert!(
        started_at.elapsed() < Duration::from_secs(5),
        "shutdown returned only after {:?}, so the remaining backoff was waited out",
        started_at.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn a_sweeper_that_makes_one_pass_then_wedges_keeps_its_backoff_growing() {
    // A sweeper that completes a single pass and then hangs for the whole stall
    // window looks "long-lived" by wall-clock alone. Treating that as a healthy
    // run would hand every replacement the initial backoff and turn a
    // slowly-wedging task into a permanent 30-second restart loop; progress
    // through the window is part of what earns the reset.
    let policy = fast_policy();
    let state = state();
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let starts = Arc::new(AtomicUsize::new(0));
    let start_times: Arc<Mutex<Vec<tokio::time::Instant>>> = Arc::new(Mutex::new(Vec::new()));
    let state_task = Arc::clone(&state);
    let starts_task = Arc::clone(&starts);
    let times_task = Arc::clone(&start_times);

    let supervisor = tokio::spawn(supervise_sweeper_task(
        state.clone(),
        shutdown_rx,
        policy,
        move || {
            let starts = Arc::clone(&starts_task);
            let times = Arc::clone(&times_task);
            let state = Arc::clone(&state_task);
            async move {
                times.lock().push(tokio::time::Instant::now());
                if starts.fetch_add(1, Ordering::SeqCst) == 0 {
                    state.record_sweeper_pass();
                }
                // One pass, then wedged for the whole stall window.
                std::future::pending::<()>().await;
            }
        },
    ));

    wait_until(
        "three replacements of a one-pass-then-wedged sweeper",
        || starts.load(Ordering::SeqCst) >= 4,
    )
    .await;
    let times = start_times.lock().clone();
    // Generation 0 completes exactly one pass and then wedges; every generation
    // after it completes none. Each death therefore waits the stall timeout plus
    // a backoff for one death more than the last. If a single completed pass were
    // enough to look "healthy", the budget would reset on every generation and
    // all three delays would be identical — which is the difference this
    // assertion exists to catch.
    let delays: Vec<_> = times.windows(2).map(|w| w[1] - w[0]).collect();
    assert!(
        delays[0] < delays[1] && delays[1] < delays[2],
        "restart delays must grow while the sweeper keeps failing without progress, \
         got {delays:?} (stall timeout {:?}, initial backoff {:?})",
        policy.stall_timeout,
        policy.initial_backoff,
    );

    drop(shutdown_tx);
    supervisor
        .await
        .expect("the supervisor task itself must not panic");
}

#[tokio::test(start_paused = true)]
async fn shutdown_stops_the_supervisor_before_it_starts_a_task() {
    // Process shutdown must not spawn a sweeper on its way out, and must stop
    // the one it owns — otherwise `main` would have to abort the task directly
    // and would bypass the supervision it just installed.
    let state = state();
    let (shutdown_tx, shutdown_rx) = watch::channel(true);
    let starts = Arc::new(AtomicUsize::new(0));
    let starts_task = Arc::clone(&starts);
    tokio::time::timeout(
        Duration::from_secs(5),
        supervise_sweeper_task(state.clone(), shutdown_rx, fast_policy(), move || {
            let starts = Arc::clone(&starts_task);
            async move {
                starts.fetch_add(1, Ordering::SeqCst);
                std::future::pending::<()>().await;
            }
        }),
    )
    .await
    .expect("an already-signalled supervisor must return, not block");
    assert_eq!(
        starts.load(Ordering::SeqCst),
        0,
        "no sweeper may be spawned after shutdown was requested"
    );
    drop(shutdown_tx);
}

/// Minimal HTTP/1.1 GET over a loopback socket: `(status, body)`.
async fn http_get(addr: std::net::SocketAddr, path: &str) -> (StatusCode, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let request = format!("GET {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.unwrap();
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, body) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("malformed HTTP response: {text:?}"));
    let code: u16 = head
        .split_whitespace()
        .nth(1)
        .unwrap_or_else(|| panic!("no status code in {head:?}"))
        .parse()
        .expect("numeric status code");
    (
        StatusCode::from_u16(code).expect("known status"),
        body.to_string(),
    )
}
