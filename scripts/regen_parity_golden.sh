#!/usr/bin/env bash
# Regenerate the §3.11 parity goldens (crates/daemon/tests/parity/*.capture).
#
# NOT part of `cargo test`: the gate compares the engine against the committed
# goldens and needs no tmux. This script is the provenance: it feeds each
# `parity/*.bin` corpus file to an independent terminal emulator (tmux, as the
# reference) and records the grid it shows, via `capture-pane -p -e -N`.
#
# This is the only remaining use of tmux in the repo, and it is a dev-time
# reference only: tmux is not a runtime or test dependency.
#
# Usage: scripts/regen_parity_golden.sh   (needs tmux on PATH; 80x24 pane)
# After running, commit the .capture files and update the version line in
# crates/daemon/tests/parity/README.md.
set -euo pipefail
cd "$(dirname "$0")/../crates/daemon/tests/parity"
sock="ldmux-parity-regen-$$"
trap 'tmux -L "$sock" kill-server 2>/dev/null || true' EXIT
for corpus in *.bin; do
  name="${corpus%.bin}"
  tmux -L "$sock" new-session -d -s s1 -x 80 -y 24 \
    "clear; cat '$PWD/$corpus'; exec sleep 9999"
  sleep 0.5
  tmux -L "$sock" capture-pane -p -e -N -t s1 > "$name.capture"
  tmux -L "$sock" kill-server
  echo "wrote $name.capture"
done
tmux -V
