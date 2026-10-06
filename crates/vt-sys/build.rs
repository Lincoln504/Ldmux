//! Links the vendored `libghostty-vt` static archive.
//!
//! The archives and headers are vendored under `packages/mobile/native/prebuilt`
//! (the same artifacts the mobile JNI shim links — one engine, one commit, two
//! consumers). The archive is chosen by the `TARGET` triple, never by what
//! happens to be in `host/`: linking another architecture's archive fails late
//! and obscurely, so an unsupported target stops here with a message.

use std::env;
use std::path::Path;

/// The prebuilt directory for a target triple, or `None` if none is vendored.
///
/// `host` is the x86_64 Linux build (what `just` and CI link); `x86_64` and
/// `arm64-v8a` are the Android builds and are not Linux-gnu targets.
fn prebuilt_dir(target: &str) -> Option<&'static str> {
    match target {
        "x86_64-unknown-linux-gnu" => Some("host"),
        _ => None,
    }
}

fn main() {
    let target = env::var("TARGET").unwrap();
    let Some(dir) = prebuilt_dir(&target) else {
        panic!(
            "ldmux-vt-sys: no vendored libghostty-vt archive for target `{target}`. \
             Supported: x86_64-unknown-linux-gnu (packages/mobile/native/prebuilt/host). \
             Vendor an archive for this target and add it to build.rs."
        );
    };
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let repo = Path::new(&manifest).join("../..");
    let prebuilt = repo.join("packages/mobile/native/prebuilt").join(dir).join("lib");
    let archive = prebuilt.join("libghostty-vt.a");

    // Fail early with a clear message if the vendored archive is missing
    // (it is committed; a clean checkout without it is a broken tree).
    if !archive.exists() {
        panic!(
            "vendored libghostty-vt.a not found at {} — the prebuilt engine \
             artifacts are not in the tree (see packages/mobile/native/prebuilt)",
            archive.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", prebuilt.display());
    println!("cargo:rustc-link-lib=static=ghostty-vt");

    // System libs the archive needs: `-lm` per the host recipe, plus
    // pthread (the vendored archive references pthread_*; on glibc >= 2.34
    // that resolves into libc, so the explicit lib is a harmless no-op there).
    println!("cargo:rustc-link-lib=dylib=m");
    println!("cargo:rustc-link-lib=dylib=pthread");

    // Re-run when the vendored artifact or the target changes.
    println!("cargo:rerun-if-changed={}", archive.display());
    println!("cargo:rerun-if-changed=build.rs");
}
