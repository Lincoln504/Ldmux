# ADR-0002: Canonical tmux geometry; clients never resize the pane

**Status:** Accepted (Phase 1 probe done: `daemon.test.ts` asserts a pinned
`120x40` grid under `window-size manual`)

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
- The `RESIZE` wire message carries the client viewport, not a tmux resize.
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