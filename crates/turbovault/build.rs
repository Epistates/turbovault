//! Build script: stamp the build's short git SHA into `TURBOVAULT_GIT_SHA`
//! so debug builds can append it to `--version` (turbovault git-sha feature).
//!
//! Best-effort: falls back to `"unknown"` when `git` or the `.git` dir is
//! unavailable (e.g. a crates.io build from the published tarball). The value
//! is only *used* on debug builds (`cfg(debug_assertions)` in `main.rs`); the
//! env var is always emitted so the `env!` reference compiles in either profile.

use std::process::Command;

fn main() {
    let sha = Command::new("git")
        .args(["rev-parse", "--short=8", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=TURBOVAULT_GIT_SHA={sha}");

    // Re-stamp when the checked-out commit moves. `.git/logs/HEAD` updates on
    // every commit/checkout/reset; `.git/HEAD` on a branch switch. Relative to
    // this crate dir (CARGO_MANIFEST_DIR = crates/turbovault) the workspace
    // `.git` is two levels up. Guarded by `exists()` so a linked-worktree
    // layout (where `.git` is a file) just degrades to a possibly-stale stamp
    // rather than a broken build.
    for p in ["../../.git/HEAD", "../../.git/logs/HEAD"] {
        if std::path::Path::new(p).exists() {
            println!("cargo:rerun-if-changed={p}");
        }
    }
}
