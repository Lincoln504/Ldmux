#!/bin/sh
# Install the host side of ldmux for the current user: the binaries, the desk-window
# launcher and the systemd --user unit. Idempotent. POSIX sh. Needs no nix to RUN;
# nix only supplies (and, as a GC root, protects) the binaries when they come from it.
#
#   nix run .#install                       # nix-built binaries (recommended)
#   FORCE=1 nix run .#install               # overwrite an existing user unit
#   scripts/install.sh [BIN_DIR]            # any directory holding ldmuxd, ldmux,
#                                           # ldmux-keeper (e.g. target/release)
#
# BIN_DIR may also come from $LDMUX_BIN_DIR. Default: ./target/release.
#
# Layout it creates (nothing outside $HOME):
#   $STATE/current                  -> the binaries. A nix-store BIN_DIR is linked here as
#                                      a nix GC root (nix-store --add-root), so
#                                      nix-collect-garbage cannot delete the glibc the
#                                      binaries link; any other BIN_DIR is copied into
#                                      $STATE/releases/<stamp>/bin and linked.
#   ~/.local/bin/{ldmuxd,ldmux,ldmux-keeper,ldmux-terminal} -> $STATE/current/bin/...
#   ~/.config/systemd/user/ldmux.service         ExecStart = the absolute $STATE/current/bin/ldmuxd.
#   ~/.config/systemd/user/ldmux-keeper.service  ExecStart = the absolute $STATE/current/bin/ldmux-keeper
#                                                (the keeper is its own unit, ADR-0021).
#
# The keeper unit and the daemon both run the binaries in one directory. An existing user
# unit is never overwritten silently: the new one is written as <unit>.new unless FORCE=1.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
bin_dir=${1:-${LDMUX_BIN_DIR:-$HERE/../target/release}}
deploy=${LDMUX_DEPLOY:-$HERE/../deploy}

for b in ldmuxd ldmux ldmux-keeper; do
  [ -x "$bin_dir/$b" ] || {
    echo "error: $bin_dir/$b not found. Build first: nix build   (or: cargo build --release --workspace)" >&2
    exit 1
  }
done
bin_dir=$(cd "$bin_dir" && pwd -P)
[ -f "$deploy/ldmux.service" ] || { echo "error: $deploy/ldmux.service not found" >&2; exit 1; }

state=${XDG_STATE_HOME:-$HOME/.local/state}/ldmux
local_bin=$HOME/.local/bin
unit_dir=${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user
mkdir -p "$state" "$local_bin" "$unit_dir"
chmod 700 "$state"

# 1. The binaries -> $state/current
case "$bin_dir" in
  /nix/store/*)
    pkg=${bin_dir%/bin}
    if command -v nix-store >/dev/null 2>&1; then
      rm -f "$state/current"
      nix-store --add-root "$state/current" --indirect --realise "$pkg" >/dev/null
    else
      echo "note: nix-store not found; linking without a GC root (nix-collect-garbage can remove it)"
      ln -sfn "$pkg" "$state/current"
    fi
    ;;
  *)
    stamp=$(date +%Y%m%d%H%M%S)
    rel=$state/releases/$stamp
    mkdir -p "$rel/bin"
    for b in ldmuxd ldmux ldmux-keeper; do
      cp "$bin_dir/$b" "$rel/bin/.$b.new" && chmod 755 "$rel/bin/.$b.new" && mv "$rel/bin/.$b.new" "$rel/bin/$b"
    done
    # The terminal launcher (a keybinding runs it; it opens your emulator and runs `ldmux shell`). In a nix
    # package it is already in bin/.
    install -m 755 "$deploy/bin/ldmux-terminal" "$rel/bin/ldmux-terminal"
    ln -sfn "$rel" "$state/current.new" && mv -T "$state/current.new" "$state/current"
    ;;
esac

# 2. ~/.local/bin links (atomic replace; never a plain file we did not make).
for b in ldmuxd ldmux ldmux-keeper ldmux-terminal; do
  [ -e "$state/current/bin/$b" ] || continue
  if [ -e "$local_bin/$b" ] && [ ! -L "$local_bin/$b" ]; then
    echo "note: replacing the regular file $local_bin/$b (from an older install) with a link"
  fi
  ln -sfn "$state/current/bin/$b" "$local_bin/.$b.new" && mv -T "$local_bin/.$b.new" "$local_bin/$b"
done

# 3. The units. The template's @BIN_DIR@ is the absolute stable path. Each unit file is
# handled on its own by the never-overwrite rule: written if absent (or FORCE=1), left
# alone when identical, else written beside it as <unit>.new. Nothing else in the unit
# directory is touched.
install_unit() {
  name=$1
  [ -f "$deploy/$name" ] || { echo "error: $deploy/$name not found" >&2; exit 1; }
  rendered=$state/$name.rendered
  sed "s|@BIN_DIR@|$state/current/bin|g" "$deploy/$name" > "$rendered"
  unit=$unit_dir/$name
  if [ ! -e "$unit" ] || [ "${FORCE:-}" = 1 ]; then
    install -m 644 "$rendered" "$unit"
    echo "installed the unit: $unit"
  elif cmp -s "$rendered" "$unit"; then
    echo "the unit is already current: $unit"
  else
    install -m 644 "$rendered" "$unit.new"
    echo "NOTE: $unit exists and differs; the new one is $unit.new"
    echo "      review it (diff \"$unit\" \"$unit.new\"), then mv it into place, or rerun with FORCE=1"
  fi
  rm -f "$rendered"
}
[ -f "$deploy/ldmux-keeper.service" ] || { echo "error: $deploy/ldmux-keeper.service not found" >&2; exit 1; }
install_unit ldmux-keeper.service
install_unit ldmux.service

if command -v systemctl >/dev/null 2>&1; then
  systemctl --user daemon-reload || echo "note: systemctl --user daemon-reload failed (no user manager in this session?)"
else
  echo "note: no systemctl; run $state/current/bin/ldmuxd yourself or use your init system"
fi

# Things that make the install work and are not ours to set.
case ":$PATH:" in
  *":$local_bin:"*) ;;
  *) echo "WARNING: $local_bin is not on PATH; add it (e.g. in ~/.profile) or the CLI will not be found" ;;
esac
if command -v loginctl >/dev/null 2>&1; then
  linger=$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null || true)
  if [ "$linger" != yes ]; then
    echo "WARNING: linger is off; the user manager (and every shell) stops at last logout. Fix: loginctl enable-linger $(id -un)"
  fi
else
  echo "note: no loginctl; make sure your user manager outlives logout"
fi
if [ ! -e "${XDG_CONFIG_HOME:-$HOME/.config}/ldmux/env" ]; then
  echo "note: no ~/.config/ldmux/env; without LDMUX_REMOTE_HOST the phone cannot connect (template: $deploy/ldmux.env.example)"
fi

# A running keeper is OLD CODE and nothing replaces it. The keeper outlives the daemon
# on purpose, so `systemctl --user restart ldmux` restarts half the system: the running
# keeper keeps executing the binary it started from (measured 2026-10-04: a keeper from
# 13:39 served a daemon built at 23:02 and the daemon silently fell back to a worse
# answer). Replacing it ends every shell (plan R3), so say so.
live_keeper=
for c in /proc/[0-9]*/comm; do
  if [ "$(cat "$c" 2>/dev/null || true)" = ldmux-keeper ]; then
    live_keeper=${c#/proc/}; live_keeper=${live_keeper%/comm}; break
  fi
done
if [ -n "$live_keeper" ]; then
  started=$(ps -o lstart= -p "$live_keeper" 2>/dev/null | xargs || true)
  echo
  echo "  WARNING: ldmux-keeper (pid $live_keeper) is STILL RUNNING THE OLD BINARY."
  echo "           started: $started"
  echo "           The keeper outlives \`systemctl --user restart ldmux\`, so the"
  echo "           daemon you are about to start will talk to the old keeper."
  echo "           Replacing it ENDS EVERY RUNNING SHELL. When that is what you"
  echo "           want:  nix run .#restart-keeper   (or scripts/restart-keeper.sh)"
  echo
fi

echo "installed ldmuxd, ldmux, ldmux-keeper and ldmux-terminal from $bin_dir"
echo "apply with: systemctl --user restart ldmux   (first time: systemctl --user enable --now ldmux)"
echo "the daemon starts ldmux-keeper.service itself; a RUNNING keeper (started by an older daemon) is replaced only by scripts/restart-keeper.sh"
