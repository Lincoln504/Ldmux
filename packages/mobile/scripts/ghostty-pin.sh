# Sourced by build-libghostty-vt.sh. The single reader of the engine pin
# (../native/ghostty.pin, which flake.nix reads too): one commit, one Zig.
#
# After sourcing: GHOSTTY_COMMIT (full 40-char SHA), GHOSTTY_ZIG_VERSION,
# and the function ghostty_checkout, which leaves $GHOSTTY_SRC at exactly the pin or dies.

PIN_FILE="${PIN_FILE:-$(cd "$(dirname "${BASH_SOURCE[0]}")/../native" && pwd)/ghostty.pin}"
[[ -f "$PIN_FILE" ]] || { echo "pin file not found: $PIN_FILE" >&2; exit 1; }

pin_get() { sed -n "s/^$1[[:space:]]\+\([^[:space:]#]\+\).*/\1/p" "$PIN_FILE" | head -1; }

GHOSTTY_COMMIT="$(pin_get commit)"
GHOSTTY_ZIG_VERSION="$(pin_get zig)"
[[ "$GHOSTTY_COMMIT" =~ ^[0-9a-f]{40}$ ]] || {
  echo "$PIN_FILE: commit must be a full 40-char SHA, got '$GHOSTTY_COMMIT'" >&2; exit 1; }

GHOSTTY_REPO="${GHOSTTY_REPO:-https://github.com/ghostty-org/ghostty}"
GHOSTTY_SRC="${GHOSTTY_SRC:-$HOME/build/ghostty}"

# Asserts the Zig on PATH is the pinned version (the flake's devShell provides it).
ghostty_check_zig() {
  local have
  have="$("$ZIG" version)"
  [[ "$have" == "$GHOSTTY_ZIG_VERSION" ]] || {
    echo "zig $have, but the pin wants $GHOSTTY_ZIG_VERSION (run inside: nix develop .#mobile)" >&2; exit 1; }
}

# Check out the pinned commit in $GHOSTTY_SRC (cloning it if absent) and assert
# `git rev-parse HEAD` equals the pin. A fetch failure is an error, not a shrug: the
# old `|| true` let a build continue on whatever happened to be checked out.
# Sets GHOSTTY_PREV_HEAD to what was checked out before, for callers that restore it.
ghostty_checkout() {
  if [[ ! -d "$GHOSTTY_SRC/.git" ]]; then
    echo "==> cloning Ghostty into $GHOSTTY_SRC"
    git init --quiet "$GHOSTTY_SRC"
    git -C "$GHOSTTY_SRC" remote add origin "$GHOSTTY_REPO"
  fi
  GHOSTTY_PREV_HEAD="$(git -C "$GHOSTTY_SRC" rev-parse HEAD 2>/dev/null || true)"
  if ! git -C "$GHOSTTY_SRC" cat-file -e "$GHOSTTY_COMMIT^{commit}" 2>/dev/null; then
    echo "==> fetching $GHOSTTY_COMMIT"
    git -C "$GHOSTTY_SRC" fetch --depth 1 origin "$GHOSTTY_COMMIT"
  fi
  git -C "$GHOSTTY_SRC" checkout --quiet --detach "$GHOSTTY_COMMIT"
  local head
  head="$(git -C "$GHOSTTY_SRC" rev-parse HEAD)"
  [[ "$head" == "$GHOSTTY_COMMIT" ]] || {
    echo "$GHOSTTY_SRC is at $head, the pin is $GHOSTTY_COMMIT" >&2; exit 1; }
}

# `zig build` flags that make dependency fetching hermetic when the flake's pre-fetched,
# hash-checked cache is available (LDMUX_GHOSTTY_ZIG_DEPS, exported by the mobile devShell).
# Without it Zig downloads the dependencies itself (by the hashes in build.zig.zon, so still
# content-checked, but needing network).
ghostty_zig_system_flags() {
  if [[ -n "${LDMUX_GHOSTTY_ZIG_DEPS:-}" && -d "$LDMUX_GHOSTTY_ZIG_DEPS" ]]; then
    printf '%s\n' --system "$LDMUX_GHOSTTY_ZIG_DEPS"
  else
    echo "note: LDMUX_GHOSTTY_ZIG_DEPS not set; zig will fetch dependencies over the network" >&2
  fi
}
