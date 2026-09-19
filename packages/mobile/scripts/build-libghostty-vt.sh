#!/usr/bin/env bash
# Build libghostty-vt for Android and link the ldmux JNI shim against it.
#
# Reproducible: takes Zig from the `mobile` devShell, the NDK from
# ANDROID_NDK_HOME, and a pinned Ghostty commit. Produces, per ABI:
#   native/prebuilt/<abi>/lib/libghostty-vt.a
#   native/prebuilt/<abi>/include/ghostty/**.h
#   composeApp/src/androidMain/jniLibs/<abi>/libldmux_terminal.so
#
# Usage: scripts/build-libghostty-vt.sh [abi ...]   (default: arm64-v8a x86_64)
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
MOBILE="$(cd "$HERE/.." && pwd)"

# Pin the engine. This is the commit verified on 2026-09-14 with Zig 0.16.0.
GHOSTTY_REPO="${GHOSTTY_REPO:-https://github.com/ghostty-org/ghostty}"
GHOSTTY_COMMIT="${GHOSTTY_COMMIT:-d5eba8d}"
GHOSTTY_SRC="${GHOSTTY_SRC:-$HOME/build/ghostty}"

ZIG="${ZIG:-$(command -v zig || true)}"
: "${ANDROID_NDK_HOME:?set ANDROID_NDK_HOME (the mobile devShell does this)}"
: "${JAVA_HOME:?set JAVA_HOME (the mobile devShell does this)}"

[[ -n "$ZIG" ]] || { echo "zig not found" >&2; exit 1; }
HOST_TAG="linux-x86_64"
NDK_BIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/bin"

abi_to_target() {
  case "$1" in
    arm64-v8a) echo "aarch64-linux-android" ;;
    x86_64)    echo "x86_64-linux-android" ;;
    armeabi-v7a) echo "armv7a-linux-androideabi" ;;
    *) echo "unknown ABI: $1" >&2; exit 1 ;;
  esac
}

abi_to_clang() {
  case "$1" in
    arm64-v8a) echo "aarch64-linux-android26-clang" ;;
    x86_64)    echo "x86_64-linux-android26-clang" ;;
    *) echo "unknown ABI: $1" >&2; exit 1 ;;
  esac
}

if [[ ! -d "$GHOSTTY_SRC/.git" ]]; then
  echo "==> cloning Ghostty into $GHOSTTY_SRC"
  git clone --depth 1 "$GHOSTTY_REPO" "$GHOSTTY_SRC"
fi
git -C "$GHOSTTY_SRC" fetch --depth 1 origin "$GHOSTTY_COMMIT" >/dev/null 2>&1 || true
git -C "$GHOSTTY_SRC" checkout --quiet "$GHOSTTY_COMMIT"

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
STAMP="$MOBILE/native/prebuilt/sources.sha256"
( cd "$MOBILE/native" && sha256sum *.c *.h | LC_ALL=C sort -k2 ) > "$STAMP"
echo "==> source stamp: $STAMP"

echo "==> done"