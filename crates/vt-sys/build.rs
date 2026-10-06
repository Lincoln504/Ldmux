//! Links the `libghostty-vt` static archive.
//!
//! The archive is built by nix from ONE pinned Ghostty commit
//! (`packages/mobile/native/ghostty.pin`; the same pin feeds the Android build, so one
//! engine, one commit, two consumers). It is found, in order:
//!
//!   1. `LDMUX_VT_LIB_DIR`: a directory containing `libghostty-vt.a`. `nix develop`
//!      and `nix build` set it. This is the supported path.
//!   2. The in-tree prebuilt directory `packages/mobile/native/prebuilt/host/lib`,
//!      if present. It is a gitignored local cache (produced by
//!      `packages/mobile/scripts/test-native.sh`), NOT part of a clone.
//!   3. Otherwise the build stops and says which nix command provides it.
//!
//! The archive is chosen for the `TARGET` triple: linking another architecture's
//! archive fails late and obscurely, so an unsupported target stops here with a
//! message. With (1) the directory is trusted to match the target (nix builds it for
//! the platform the derivation is built on); the in-tree fallback is x86_64 Linux only.

use std::env;
use std::path::{Path, PathBuf};

/// The in-tree prebuilt directory for a target triple, or `None` if there is none.
///
/// `host` is the x86_64 Linux build; `x86_64` and `arm64-v8a` are the Android builds and
/// are not Linux-gnu targets.
fn prebuilt_dir(target: &str) -> Option<&'static str> {
    match target {
        "x86_64-unknown-linux-gnu" => Some("host"),
        _ => None,
    }
}

fn find_archive(target: &str, manifest: &str) -> Result<PathBuf, String> {
    if let Some(dir) = env::var_os("LDMUX_VT_LIB_DIR").filter(|d| !d.is_empty()) {
        let dir = PathBuf::from(dir);
        let archive = dir.join("libghostty-vt.a");
        return if archive.is_file() {
            Ok(dir)
        } else {
            Err(format!(
                "LDMUX_VT_LIB_DIR={} does not contain libghostty-vt.a. \
                 Unset it, or point it at a directory built by `nix build .#libghostty-vt` \
                 (the archive is in the `dev` output's lib/).",
                dir.display()
            ))
        };
    }
    if let Some(sub) = prebuilt_dir(target) {
        let dir = Path::new(manifest)
            .join("../../packages/mobile/native/prebuilt")
            .join(sub)
            .join("lib");
        if dir.join("libghostty-vt.a").is_file() {
            return Ok(dir);
        }
    }
    Err(format!(
        "ldmux-vt-sys: libghostty-vt.a was not found for target `{target}`.\n\
         The engine is built by nix from the commit pinned in packages/mobile/native/ghostty.pin. \
         Either:\n\
         \x20 - enter the dev shell, which builds it and sets LDMUX_VT_LIB_DIR:  nix develop\n\
         \x20 - or export it yourself:  export LDMUX_VT_LIB_DIR=\"$(nix build .#libghostty-vt.dev --no-link --print-out-paths)/lib\"\n\
         \x20 - or build everything with nix:  nix build\n\
         (An in-tree packages/mobile/native/prebuilt/host/lib is also accepted on x86_64 Linux; \
         it is a local cache and is not part of a clone.)"
    ))
}

fn main() {
    let target = env::var("TARGET").unwrap();
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap();
    println!("cargo:rerun-if-env-changed=LDMUX_VT_LIB_DIR");
    println!("cargo:rerun-if-changed=build.rs");

    let lib_dir = match find_archive(&target, &manifest) {
        Ok(d) => d,
        Err(msg) => panic!("{msg}"),
    };
    let archive = lib_dir.join("libghostty-vt.a");

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    println!("cargo:rustc-link-lib=static=ghostty-vt");

    // System libs the archive needs: `-lm`, plus pthread (the archive references
    // pthread_*; on glibc >= 2.34 that resolves into libc, so the explicit lib is a
    // harmless no-op there).
    println!("cargo:rustc-link-lib=dylib=m");
    println!("cargo:rustc-link-lib=dylib=pthread");

    // Re-link when the archive changes.
    println!("cargo:rerun-if-changed={}", archive.display());
}
