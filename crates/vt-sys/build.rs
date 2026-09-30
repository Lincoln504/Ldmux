//! Links the vendored `libghostty-vt` static archive.
//!
//! The archive and headers are vendored under `packages/mobile/native/prebuilt/host`
//! (the same artifact the mobile JNI shim links — one engine, one commit, two
//! consumers). The host link recipe is `packages/mobile/scripts/test-native.sh`,
//! which compiles with `-lm` only; we mirror it here.

use std::env;
use std::path::Path;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    let repo = Path::new(&manifest).join("../..");
    let prebuilt = repo
        .join("packages/mobile/native/prebuilt/host")
        .join("lib");
    let archive = prebuilt.join("libghostty-vt.a");

    // The vendored static archive, by name.
    println!("cargo:rustc-link-search=native={}", prebuilt.display());
    println!("cargo:rustc-link-lib=static=ghostty-vt");

    // System libs the archive needs: `-lm` per the host recipe, plus
    // pthread (the vendored archive references pthread_*; on glibc >= 2.34
    // that resolves into libc, so the explicit lib is a harmless no-op there).
    println!("cargo:rustc-link-lib=dylib=m");
    println!("cargo:rustc-link-lib=dylib=pthread");

    // Re-run when the vendored artifact changes.
    println!(
        "cargo:rerun-if-changed={}",
        archive.display()
    );

    // Fail early with a clear message if the vendored archive is missing
    // (it is committed; a clean checkout without it is a broken tree).
    if !archive.exists() {
        panic!(
            "vendored libghostty-vt.a not found at {} — the prebuilt engine \
             artifacts are not in the tree (see packages/mobile/native/prebuilt)",
            archive.display()
        );
    }
}
