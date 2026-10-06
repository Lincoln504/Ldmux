# ADR-0002: Canonical tmux geometry; clients never resize the pane

**Status:** Superseded by [ADR-0018](0018-session-layer.md) (2026-10-04); the keeper is now the canonical geometry. Kept as history.

> **Superseded (2026-10-04, D4).** The tmux session layer this ADR decided on is gone: `crates/keeper` owns the PTYs and runs one libghostty-vt per session, and the daemon is Rust. Kept for *why*, not as current design. See ADR-0018 and `docs/archive/RUST-PORT-LOG.md`.

> The keeper is now the canonical geometry: a claim hands the size to the device
> that holds the shell, and `protocol::clamp_pane_size` bounds it.

## Context

tmux maintains **one window size per window**. If any attached client influences
sizing (`window-size latest/smallest`, or a control-mode client calling
`refresh-client -C`), the window reflows for **every** client. The v0.1 spec had a
viewing client "claim the viewport", which would reflow the desktop `alacritty`
mirror whenever the phone attached — defeating the synchronized-view goal (G5).

dooboxes enforces a deliberate multi-client attachment model (1 desktop + 1
mobile, with an exclusive lock) precisely to avoid geometry fights.

## Decision

- Set `set-option -g window-size manual` and pin a **canonical geometry**
  (default: match the desktop display, e.g. detect → `120x40`; configurable).
- The mobile client renders a **viewport over the canonical grid** (scale/pan/
  pinch); it **never** resizes the tmux pane and never calls `refresh-client -C`.
- There is no resize message at all: the client scales the canonical grid in
  CSS and sends no geometry (the v0.1 `RESIZE` message was cut — see
  `docs/SPEC.md` §6.2).
- Independent per-client sizes are only available via **grouped sessions**
  (`new-session -t`), which is a separate, explicitly non-synced mode; out of scope.

Phase 1 includes a **probe** on tmux 3.5a confirming `window-size manual`
behavior and the default-size detection.

## Consequences

- **+** Phone and desktop mirror can view the same session simultaneously without
  reflow fights (G5, FR-12).
- **+** The desktop mirror is usable at desktop geometry.
- **−** The phone must scale/pan a grid sized for the desktop; very narrow panes
  from mobile-side apps are not supported in synced mode.