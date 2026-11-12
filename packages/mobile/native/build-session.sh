#!/usr/bin/env bash
# Build libldmux_session.so: the Rust session (crates/session-ffi) plus its JNI shim.
#
#   build-session.sh host            -> native/build/host/libldmux_session.so  (JVM unit tests)
#   build-session.sh android         -> composeApp/src/androidMain/jniLibs/<abi>/libldmux_session.so
#
# Why a second library and not a line in CMakeLists.txt: libldmux_terminal.so is produced by the
# libghostty pipeline and shipped prebuilt; linking a Rust staticlib into it would make every
# session change rebuild libghostty and make the terminal's build depend on a Rust toolchain.
# The seam is the same (C core, JNI shim, handle in/out), only the artefact is separate.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../../.." && pwd)"
mode="${1:?usage: build-session.sh host|android}"
sdk="${ANDROID_HOME:-$HOME/Android/Sdk}"

build_rust() { # target
  local target="${1:-}" cargo_bin=cargo
  # The devshell's rustc is nix-pinned and carries only the host std, so a cross build for
  # Android fails with "can't find crate for `std`" and points at `rustup target add` for a
  # toolchain it is not using. rustup's toolchain has both Android targets installed; use its
  # cargo for the cross build and keep the pinned one for the host build, which is where
  # reproducibility actually matters.
  # **RUSTC as well as cargo**: cargo invoked by absolute path still resolves `rustc` from
  # PATH, and in the devshell PATH is the nix rustc first. Pinning only the cargo binary left
  # the nix rustc doing the work, which has no Android std: "can't find crate for `core`".
  if [ -n "$target" ] && command -v rustup >/dev/null 2>&1; then
    cargo_bin="$(rustup which cargo)"
    (cd "$root" && RUSTC="$(rustup which rustc)" "$cargo_bin" build --release --offline \
      -p ldmux-session-ffi --target "$target")
    return
  fi
  (cd "$root" && "$cargo_bin" build --release --offline -p ldmux-session-ffi ${target:+--target "$target"})
}

case "$mode" in
host)
  jdk="${JAVA_HOME:?JAVA_HOME must point at a JDK (for jni.h)}"
  build_rust ""
  out="$here/build/host"; mkdir -p "$out"
  cc -shared -fPIC -O2 -Wall -Wextra -I"$jdk/include" -I"$jdk/include/linux" -I"$here" \
    "$here/ldmux_client_jni.c" \
    "$root/target/release/libldmux_session_ffi.a" \
    -lpthread -ldl -lm -o "$out/libldmux_session.so"
  echo "$out/libldmux_session.so"
  ;;
android)
  ndk="$(ls -d "$sdk"/ndk/*/ | sort -V | tail -1)"
  bin="$(echo "$ndk"toolchains/llvm/prebuilt/*/bin)"
  for pair in aarch64-linux-android:arm64-v8a x86_64-linux-android:x86_64; do
    triple="${pair%%:*}"; abi="${pair##*:}"
    build_rust "$triple"
    dest="$here/../composeApp/src/androidMain/jniLibs/$abi"; mkdir -p "$dest"
    "$bin/${triple}28-clang" -shared -fPIC -O2 -Wall -Wextra -I"$here" \
      "$here/ldmux_client_jni.c" \
      "$root/target/$triple/release/libldmux_session_ffi.a" \
      -llog -lm -ldl -Wl,-z,max-page-size=16384 -Wl,--gc-sections -o "$dest/libldmux_session.so"
    echo "$dest/libldmux_session.so"
  done
  ;;
*) echo "usage: build-session.sh host|android" >&2; exit 2 ;;
esac
