#!/usr/bin/env bash
# Compile and run the portable shim's own tests.
#
# These run on the *host*, against the host build of libghostty-vt that nix produces
# from the pinned commit (native/ghostty.pin), so they need no emulator, no phone, no
# daemon and no Ghostty checkout. The engine and the shim are the same sources as the
# Android build, so what is true here is true on the device — and unlike a device test,
# the answer is visible in a terminal instead of inferred from a screenshot.
#
# Run inside a nix devShell (`nix develop` or `nix develop .#mobile`): they export
# LDMUX_VT_LIB_DIR (the archive) and LDMUX_VT_INCLUDE_DIR (the headers of that same build).
#
# Usage: nix develop .#mobile -c packages/mobile/scripts/test-native.sh
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
MOBILE="$(cd "$HERE/.." && pwd)"

: "${LDMUX_VT_LIB_DIR:?not set: run inside \`nix develop\` (it builds the engine from the pin)}"
: "${LDMUX_VT_INCLUDE_DIR:?not set: run inside \`nix develop\`}"
[[ -f "$LDMUX_VT_LIB_DIR/libghostty-vt.a" ]] || { echo "no libghostty-vt.a in $LDMUX_VT_LIB_DIR" >&2; exit 1; }

CC_BIN="${CC:-$(command -v cc || command -v zig || true)}"
[[ -n "$CC_BIN" ]] || { echo "no C compiler found (run inside: nix develop)" >&2; exit 1; }
CC_CMD=("$CC_BIN")
[[ "$(basename "$CC_BIN")" == zig ]] && CC_CMD=("$CC_BIN" cc)

BUILD="${LDMUX_NATIVE_BUILD:-$MOBILE/native/build}"
mkdir -p "$BUILD"
NATIVE="$MOBILE/native"
SRC_CORE=("$NATIVE/ldmux_terminal.c")
ENGINE_LIB="$LDMUX_VT_LIB_DIR/libghostty-vt.a"
INC="$LDMUX_VT_INCLUDE_DIR"

# Sanitizers. `zig cc` cannot link AddressSanitizer (it ships no runtime), so the
# instrumented build needs a real system C compiler: the shim and the tests are
# instrumented, the engine archive is not. LDMUX_NATIVE_SANITIZE=0 turns it off.
SAN="${LDMUX_NATIVE_SANITIZE:-1}"
SAN_FLAGS=()
if [[ "$SAN" == 1 && "$(basename "$CC_BIN")" != zig ]]; then
  SAN_FLAGS=(-fsanitize=address,undefined -fno-sanitize-recover=undefined -fno-omit-frame-pointer)
  echo "==> sanitizers: address,undefined ($CC_BIN)"
else
  echo "==> sanitizers: off (needs a non-zig C compiler and LDMUX_NATIVE_SANITIZE!=0)"
fi
compile() { # compile <out> <sources...>
  local out="$1"; shift
  "${CC_CMD[@]}" -O1 -g -std=c11 -Wall -Wextra "${SAN_FLAGS[@]}" \
    -o "$out" -I"$INC" "$@" "$ENGINE_LIB" -lm -lpthread
}

echo "==> compiling the shim tests"
compile "$BUILD/test_engine" "$NATIVE/test_engine.c" "${SRC_CORE[@]}"

# The JNI binding's argument handling, against a fake JNIEnv. Needs a jni.h: the
# JDK's, else the NDK's. Skipped (loudly) when neither is present.
JNI_INC=""
JAVA_BIN="$(command -v java 2>/dev/null || true)"
for d in "${JAVA_HOME:-}" "${JAVA_BIN:+$(dirname "$(dirname "$(readlink -f "$JAVA_BIN")")")}"; do
  [[ -n "$d" && -f "$d/include/jni.h" ]] && JNI_INC="$d/include" && break
done
if [[ -z "$JNI_INC" ]]; then
  for d in "${ANDROID_NDK_HOME:-}" "${ANDROID_HOME:-/nonexistent}"/ndk/*; do
    f="$(ls "$d"/toolchains/llvm/prebuilt/*/sysroot/usr/include/jni.h 2>/dev/null | head -1 || true)"
    [[ -n "$f" ]] && JNI_INC="$(dirname "$f")" && break
  done
fi
RC=0
if [[ -n "$JNI_INC" ]]; then
  JNI_MD=()
  [[ -d "$JNI_INC/linux" ]] && JNI_MD=(-I"$JNI_INC/linux")
  echo "==> compiling the JNI tests (jni.h from $JNI_INC)"
  compile "$BUILD/test_jni" -I"$JNI_INC" "${JNI_MD[@]}" -I"$NATIVE" \
    "$NATIVE/test_jni.c" "$NATIVE/ldmux_jni.c" "${SRC_CORE[@]}"
else
  echo "==> SKIPPED the JNI tests: no jni.h (set JAVA_HOME or ANDROID_NDK_HOME)" >&2
fi

# Target-neutrality of the portable core, object-only so no SDK is needed. Zig
# bundles the macOS libc headers, so aarch64-macos is a real second target and the
# stand-in for iOS (same compiler, Apple ABI, arm64). aarch64-ios itself needs the iOS
# SDK's headers (`stdlib.h` is not found without one), so it is not attempted. The core
# must stay warning-free under -Werror there too.
ZIG_BIN="${ZIG:-$(command -v zig || true)}"
if [[ -n "$ZIG_BIN" ]]; then
  echo "==> object-only compile: core, -Werror, aarch64-macos"
  "$ZIG_BIN" cc -target aarch64-macos -std=c11 -Wall -Wextra -Werror -O2 -c \
    -I"$INC" "$NATIVE/ldmux_terminal.c" -o "$BUILD/ldmux_terminal.aarch64-macos.o"
else
  echo "==> SKIPPED the aarch64-macos object compile: no zig" >&2
fi

echo "==> running the shim tests"
"$BUILD/test_engine" || RC=1
if [[ -x "$BUILD/test_jni" ]]; then
  echo "==> running the JNI tests"
  "$BUILD/test_jni" || RC=1
fi
exit "$RC"
