#!/usr/bin/env bash
# Compile and run the portable shim's own tests.
#
# These run on the *host*, against a host build of libghostty-vt, so they need no
# emulator, no phone and no daemon. The engine and the shim are the same sources
# as the Android build, so what is true here is true on the device — and unlike a
# device test, the answer is visible in a terminal instead of inferred from a
# screenshot.
#
# Needs the `mobile` devShell (for zig). The host engine is built once and cached
# under native/prebuilt/host; subsequent runs are seconds.
#
# Usage: scripts/test-native.sh
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
MOBILE="$(cd "$HERE/.." && pwd)"

GHOSTTY_SRC="${GHOSTTY_SRC:-$HOME/build/ghostty}"
GHOSTTY_COMMIT="${GHOSTTY_COMMIT:-d5eba8d}"
ZIG="${ZIG:-$(command -v zig || true)}"

[[ -n "$ZIG" ]] || { echo "zig not found (run inside: nix develop .#mobile)" >&2; exit 1; }
[[ -d "$GHOSTTY_SRC/.git" ]] || { echo "ghostty source not found at $GHOSTTY_SRC" >&2; exit 1; }

OUT="$MOBILE/native/prebuilt/host"
if [[ ! -f "$OUT/lib/libghostty-vt.a" ]]; then
  echo "==> building libghostty-vt for the host (once)"
  git -C "$GHOSTTY_SRC" checkout --quiet "$GHOSTTY_COMMIT"
  ( cd "$GHOSTTY_SRC" && "$ZIG" build -Demit-lib-vt -Doptimize=ReleaseSafe --prefix "$OUT" )
fi

BIN="$MOBILE/native/build/test_engine"
mkdir -p "$(dirname "$BIN")"

echo "==> compiling the shim tests"
"$ZIG" cc -O1 -std=c11 -o "$BIN" \
  -I"$OUT/include" \
  "$MOBILE/native/test_engine.c" \
  "$MOBILE/native/ldmux_terminal.c" \
  "$OUT/lib/libghostty-vt.a" \
  -lm

echo "==> running"
exec "$BIN"
