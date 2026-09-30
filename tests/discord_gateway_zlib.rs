//! Regression guard for the Discord gateway decompression backend (#1531, #1535).
//!
//! serenity decompresses every gateway payload through `flate2`. On x86_64 macOS
//! release builds the default `rust_backend` (miniz_oxide + simd-adler32) is
//! implicated in post-connect SIGSEGVs reported for this binary. The `discord`
//! feature of `openab-core` must therefore force flate2's `zlib` backend so the
//! operating system's zlib inflates gateway payloads instead of the pure-Rust
//! decompressor.

use std::path::Path;

fn workspace_file(rel: &str) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(root.join(rel)).unwrap_or_else(|e| panic!("failed to read {rel}: {e}"))
}

/// openab-core's `discord` feature must enable its `flate2` dependency and the
/// dep must request the `zlib` backend feature, so any discord-enabled build
/// unifies flate2 with `any_c_zlib` (which outranks `rust_backend` in flate2's
/// ffi selection).
#[test]
fn discord_feature_enables_flate2_zlib_backend() {
    let manifest = workspace_file("crates/openab-core/Cargo.toml");

    let discord_line = manifest
        .lines()
        .find(|l| l.trim_start().starts_with("discord"))
        .expect("openab-core manifest must declare a `discord` feature");
    assert!(
        discord_line.contains("dep:flate2"),
        "the `discord` feature must enable `dep:flate2` so the zlib backend \
         is active whenever serenity is: {discord_line}"
    );

    let flate2_decl = manifest
        .lines()
        .find(|l| l.trim_start().starts_with("flate2") && l.contains("zlib"));
    assert!(
        flate2_decl.is_some(),
        "openab-core must declare flate2 with the `zlib` feature for Discord \
         gateway decompression"
    );
}

/// The resolved lockfile must contain `libz-sys`: that crate is only pulled in
/// when flate2's `zlib` feature is enabled, and its presence proves the C zlib
/// backend is actually selected for the gateway decompressor.
#[test]
fn lockfile_resolves_system_zlib_for_discord() {
    let lock = workspace_file("Cargo.lock");
    assert!(
        lock.contains("name = \"libz-sys\""),
        "Cargo.lock must resolve libz-sys so flate2 uses the system zlib \
         backend instead of miniz_oxide for gateway decompression"
    );
}
