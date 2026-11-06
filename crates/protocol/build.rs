//! The build's identity, as one string.
//!
//! **Why a build script for one string.** Nothing in the tree could say which build was running:
//! `ldmux ping` answered with the *protocol* version, the daemon's startup line named paths, and
//! the desk windows on this machine were running an install two generations older than the
//! daemon's with no way to see it (RELEASE-AUDIT 14.2). The identity has to be resolved where the
//! source is — at build time — because at run time the checkout may be gone, and a build that
//! cannot name itself is a claim nobody can check.
//!
//! It emits no code; it sets one `rustc-env` value (`LDMUX_BUILD_ID`), which
//! `ldmux_protocol::BUILD_ID` exposes. Precedence:
//!
//! 1. `LDMUX_BUILD_ID` in the environment (a release pipeline that knows its own name);
//! 2. the git short sha of `HEAD`, with `-dirty` when the worktree has changes;
//! 3. `unknown` — a nix sandbox has no `.git`, and a build that cannot say still has to build.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-env-changed=LDMUX_BUILD_ID");
    println!("cargo:rerun-if-changed=build.rs");
    // `HEAD` moves on every commit; the index and the worktree move the `-dirty` suffix. None of
    // these exist in the nix sandbox, where the value is `unknown` whatever changes.
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/index");

    let id = std::env::var("LDMUX_BUILD_ID")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(git_id);
    println!("cargo:rustc-env=LDMUX_BUILD_ID={id}");
}

/// `<short sha>` or `<short sha>-dirty`, else `unknown`.
fn git_id() -> String {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    };
    let Some(sha) = git(&["rev-parse", "--short=8", "HEAD"]) else {
        return "unknown".to_string();
    };
    // `--porcelain` is empty for a clean tree. Untracked files (`--untracked-files=no`) do not
    // change what was compiled, so they do not make a build "dirty".
    let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    if dirty { format!("{sha}-dirty") } else { sha }
}
