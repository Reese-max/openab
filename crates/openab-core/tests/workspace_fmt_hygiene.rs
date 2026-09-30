use std::path::Path;
use std::process::Command;

/// Regression guard: the workspace must stay `cargo fmt --all -- --check`
/// clean. Nothing in CI enforces rustfmt today, so the tree drifted once
/// already; this test turns the drift into a failing build.
#[test]
fn workspace_is_rustfmt_clean() {
    // CARGO_MANIFEST_DIR = crates/openab-core → two levels up is the
    // workspace root, matching where CI/the verifier runs the command.
    let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root");

    // rustfmt is an optional toolchain component — skip where absent
    // (e.g. CI jobs that only install `clippy`) instead of failing hard.
    let probe = Command::new("cargo")
        .args(["fmt", "--version"])
        .current_dir(workspace_root)
        .output()
        .expect("failed to spawn cargo fmt");
    if !probe.status.success() {
        eprintln!("cargo fmt not installed; skipping rustfmt cleanliness check");
        return;
    }

    let output = Command::new("cargo")
        .args(["fmt", "--all", "--", "--check"])
        .current_dir(workspace_root)
        .output()
        .expect("failed to spawn cargo fmt");
    assert!(
        output.status.success(),
        "cargo fmt --all -- --check failed:\n{}",
        String::from_utf8_lossy(&output.stdout)
    );
}
