# ADR-0028 — The desk surface: attach in your own terminal, and spawn only when ldmux must

**Status:** Accepted (2026-11-06).

- **Date:** 2026-11-06
- **Related:** `crates/attach`, `crates/daemon/src/desktop.rs`, `crates/daemon/src/bin/ldmux.rs`,
  `deploy/bin/ldmux-terminal`, ADR-0010 (ownership), ADR-0021 (scopes)

## Context

There are two ways to look at a shell from the machine it runs on: run the attach
client in a terminal you already have open, or have ldmux open a terminal for you.

Both existed, with no rule about which to use. The verbs overlapped (`run`, `window`,
`live`, `mirror`, `unlive`), the keybinding wrapper carried its own copy of the terminal
selection order, and the daemon preferred a hard-coded list of emulators over the terminal
the user actually configured. On a machine whose default is GNOME Terminal the "send to
desktop" from the phone could open something the user never uses, and on a bare i3 session
the list was right only by accident.

## Decision

1. **`ldmux attach <id>` is the desk client for a terminal you already have.** It runs the
   attach client in the current terminal and becomes the pane; `ldmux run` is the same for a
   shell that does not exist yet. These are the verbs a person uses on the host.
2. **A spawned window is for the cases where nobody is sitting at the terminal already**:
   the phone's "send to desktop", and the `Mod+Return` keybinding. Those keep working, and
   `ldmux window` is the command they use.
3. **A spawned window opens in the terminal the user configured**, in this order: a pin set
   with `ldmux terminal set`, `$TERMINAL`, `xdg-terminal-exec` when installed, **the
   configured terminal of the desktop session that is actually running** (GNOME/Cinnamon/MATE
   `gsettings`, KDE `kdeglobals`, XFCE `xfconf` and `helpers.rc`), an emulator already running
   in this session, the system `x-terminal-emulator` alternative, and only then the static
   list. An indirection is always followed: `x-terminal-emulator` resolves through its symlink
   and its `.wrapper` shim to the program that opens, and a `.desktop` id or a probe's prose is
   not an answer.
4. **A desktop setting is only read on that desktop.** Reading a GNOME schema on an XFCE
   session is what made this machine answer `x-terminal-emulator` (the Debian alternative)
   while ten alacritty windows were open; on a bare window manager no desktop key is read at
   all.
5. **No bespoke variables.** `LDMUX_TERMINAL` and `LDMUX_TERMINAL_CMD` are removed: no new
   user would find them. The discoverable answer is `ldmux terminal set <program>`, which
   writes one line to `$XDG_CONFIG_HOME/ldmux/terminal`; `$TERMINAL` and `xdg-terminal-exec`
   are honoured because other tools already set them. An emulator that needs unusual flags is
   wrapped in a script and named by the pin or `$TERMINAL`.
6. **Every spawned window gets its own transient scope** under `ldmux-windows.slice`,
   mirroring the keeper's shell scopes, with a silent fallback when there is no user bus. A
   window in the daemon's cgroup made the unit's memory figure meaningless, flooded the
   journal on every restart and outlived a `systemctl --user stop`.
7. **A window that cannot be opened says why.** A missing emulator, a mistyped `$TERMINAL`
   and an emulator that forks and exits are three different failures and each names itself;
   the wrapper falls back to a plain terminal only when the daemon is not there to ask.

## Consequences

- The host surface is one rule: attach in a terminal you have, spawn when the request came
  from somewhere other than a terminal.
- The phone's "send to desktop" lands in the user's own terminal, on any desktop, without
  configuration.
- The wrapper no longer duplicates the emulator list; it asks the daemon, and its fallback
  exists only for "the daemon is down".
- `ldmux --help` lists verbs a person can use; the placement adapters (i3, sway, hyprland)
  remain optional and are still allowed to fail to "opened, not placed".
