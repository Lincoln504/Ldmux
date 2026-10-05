# §3.11 parity goldens

`NN-name.bin` is a corpus (raw bytes fed to a terminal, `\r\n` line ends, 80x24).
`NN-name.capture` is the grid an independent reference emulator showed for it.

- Reference: **tmux 3.5a**, `capture-pane -p -e -N`, captured **2026-10-05**.
- Consumer: `crates/daemon/tests/parity.rs` (`golden_corpus_cells_match_the_reference_grids`)
  decodes each `.capture` with `tests/sgrep` and compares it cell by cell to our
  engine's grid. No tmux is needed at test time.
- Regenerate (deliberate, manual, needs tmux): `scripts/regen_parity_golden.sh`,
  then commit the `.capture` files and update the version/date above.
- Overline (SGR 53) is not in the corpus: tmux 3.5a emits `ESC[5:3m` for it.
