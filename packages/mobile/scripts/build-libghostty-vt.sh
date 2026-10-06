#!/usr/bin/env bash
# Build libghostty-vt for Android and link the ldmux JNI shim against it.
#
# Reproducible: takes Zig (pinned version) from the `mobile` devShell, the NDK from
# ANDROID_NDK_HOME, and the Ghostty commit pinned in native/ghostty.pin (the same pin the
# Rust build uses through nix). The checkout is asserted to be exactly the pin, and the
# zig dependency cache comes from the flake (LDMUX_GHOSTTY_ZIG_DEPS) when set. Produces,
# per ABI:
#   native/prebuilt/<abi>/lib/libghostty-vt.a
#   native/prebuilt/<abi>/include/ghostty/**.h
#   composeApp/src/androidMain/jniLibs/<abi>/libldmux_terminal.so
#
# Usage: scripts/build-libghostty-vt.sh [abi ...]   (default: arm64-v8a x86_64)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
MOBILE="$(cd "$HERE/.." && pwd)"

# shellcheck source=ghostty-pin.sh
source "$HERE/ghostty-pin.sh"

ZIG="${ZIG:-$(command -v zig || true)}"
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME (the mobile devShell does this)}"
: "${JAVA_HOME:?set JAVA_HOME (the mobile devShell does this)}"

[[ -n "$ZIG" ]] || { echo "zig not found (run inside: nix develop .#mobile)" >&2; exit 1; }
ghostty_check_zig
# Derived, not hardcoded. The flake advertises x86_64-linux, aarch64-linux and
# aarch64-darwin, and the NDK names its toolchain directory per host — so the old
# `HOST_TAG="linux-x86_64"` built on exactly one of the three and failed with a
# missing $NDK_BIN on the other two, including the darwin one the flake is kept for.
case "$(uname -s)" in
  Linux)  HOST_OS="linux" ;;
  Darwin) HOST_OS="darwin" ;;
  *) echo "unsupported build host: $(uname -s)" >&2; exit 1 ;;
 esac
case "$(uname -m)" in
  x86_64)        HOST_ARCH="x86_64" ;;
  arm64|aarch64) HOST_ARCH="aarch64" ;;
  *) echo "unsupported build host arch: $(uname -m)" >&2; exit 1 ;;
 esac
HOST_TAG="$HOST_OS-$HOST_ARCH"
[[ -d "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG" ]] || {
  echo "no NDK toolchain for this host ($HOST_TAG) under $ANDROID_NDK_HOME" >&2
  echo "available: $(ls "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt" 2>/dev/null | tr '\n' ' ')" >&2
  exit 1
}
NDK_BIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/bin"

abi_to_target() {
  case "$1" in
    arm64-v8a) echo "aarch64-linux-android" ;;
    x86_64)    echo "x86_64-linux-android" ;;
    armeabi-v7a) echo "armv7a-linux-androideabi" ;;
    *) echo "unknown ABI: $1" >&2; exit 1 ;;
  esac
}

# The API level is a build input, so it is named once and recorded. It is deliberately
# below `minSdk` (28 in build.gradle.kts): compiling against an older API is safe, and it
# keeps the library loadable on anything the app can install on. It was the literal `26`
# in two places here and in no stamp, so it would silently stay at 26 if minSdk moved.
ANDROID_API="${ANDROID_API:-26}"

abi_to_clang() {
  case "$1" in
    arm64-v8a) echo "aarch64-linux-android${ANDROID_API}-clang" ;;
    x86_64)    echo "x86_64-linux-android${ANDROID_API}-clang" ;;
    *) echo "unknown ABI: $1" >&2; exit 1 ;;
  esac
}

ghostty_checkout
mapfile -t ZIG_SYSTEM < <(ghostty_zig_system_flags)

ABIS=("$@")
[[ ${#ABIS[@]} -gt 0 ]] || ABIS=(arm64-v8a x86_64)

JNI_INC="$JAVA_HOME/include"
JNI_INC_LINUX="$JAVA_HOME/include/linux"

for ABI in "${ABIS[@]}"; do
  TARGET="$(abi_to_target "$ABI")"
  OUT="$MOBILE/native/prebuilt/$ABI"
  mkdir -p "$OUT"

  echo "==> building libghostty-vt for $ABI ($TARGET)"
  ( cd "$GHOSTTY_SRC" && "$ZIG" build \
      -Demit-lib-vt \
      -Dtarget="$TARGET" \
      -Doptimize=ReleaseSafe \
      "${ZIG_SYSTEM[@]}" \
      --prefix "$OUT" )

  echo "==> linking libldmux_terminal.so for $ABI"
  JNI_DIR="$MOBILE/composeApp/src/androidMain/jniLibs/$ABI"
  mkdir -p "$JNI_DIR"
  "$NDK_BIN/$(abi_to_clang "$ABI")" \
    -shared -fPIC -O2 -std=c11 \
    -I"$OUT/include" -I"$JNI_INC" -I"$JNI_INC_LINUX" \
    -o "$JNI_DIR/libldmux_terminal.so" \
    "$MOBILE/native/ldmux_terminal.c" \
    "$MOBILE/native/ldmux_jni.c" \
    "$OUT/lib/libghostty-vt.a" \
    -llog -lm
  echo "    $(stat -c%s "$JNI_DIR/libldmux_terminal.so") bytes -> $JNI_DIR"
done

# A stamp of the shim sources this build compiled, so the freshness guard can ask
# "is the packaged library built from this source" instead of "is it newer than it".
# Timestamps answer the second question and it is the wrong one: re-saving a file
# without changing it makes every packaged library look stale, and a checkout
# rewrites them all.
#
# The files are named, not globbed. `sha256sum *.c *.h` swept up test_engine.c,
# which is the *host* suite — it is linked into no Android library anywhere — so
# editing a host test claimed the packaged .so was stale and demanded a full NDK
# rebuild to clear a warning about a file that is not in the artefact. A stamp has
# to be a list of build inputs, and a build input is a thing you can point at in
# the build command.
STAMP="$MOBILE/native/prebuilt/sources.sha256"
SHIM_SOURCES="ldmux_terminal.h ldmux_terminal.c ldmux_jni.c"
( cd "$MOBILE/native" && sha256sum $SHIM_SOURCES | LC_ALL=C sort -k2 ) > "$STAMP"
echo "==> source stamp: $STAMP"

# The *other* two build inputs, and the reason this file exists separately: changing
# `GHOSTTY_COMMIT` swaps the entire emulator the library wraps, and changing the API level
# changes the compiled-in platform, and neither was recorded anywhere — so the freshness
# guard reported a library built from the old engine as fresh. It cannot go in
# `sources.sha256`, which is a list of file hashes and is read as one.
BUILD_INPUTS="$MOBILE/native/prebuilt/build-inputs.txt"
{
  printf 'engine-commit %s\n' "$GHOSTTY_COMMIT"
  printf 'zig %s\n' "$GHOSTTY_ZIG_VERSION"
  printf 'api-level %s\n' "$ANDROID_API"
  # The archives themselves, so "built from the pinned engine" is checkable from the
  # recorded bytes and not only from the recorded intent.
  for ABI in "${ABIS[@]}"; do
    printf 'archive-sha256 %s %s\n' "$ABI" \
      "$(sha256sum "$MOBILE/native/prebuilt/$ABI/lib/libghostty-vt.a" | cut -d' ' -f1)"
  done
} > "$BUILD_INPUTS"
echo "==> build inputs: $BUILD_INPUTS"

echo "==> done"
