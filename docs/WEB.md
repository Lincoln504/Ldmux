# ldmux web client

`packages/web/public` is the **reference terminal client** — a buildless static
bundle (HTML + CSS + vendored xterm.js) served by the daemon itself. On a phone
it is the whole product today; on Android it becomes the payload of the
`TerminalSurface` WebView seam, so the rendering code is written once.

There is **no build step and no runtime dependency**: xterm.js 6.0.0 and
`@xterm/addon-fit` 0.11.0 are vendored as UMD files under `public/vendor/`
(licenses included). This is consistent with the repo-wide "buildless" rule; the
only third-party code is a static asset the daemon serves.

## Serving

`RemoteServer` serves `GET /` (and any non-`/ws` path) from the daemon's
`webRoot`, which defaults to `packages/web/public`. Paths are normalized and
confined to the root (a request that escapes it is `403`; a missing file is
`404`). `POST`-style APIs do not exist — all interaction is the WebSocket at
`/ws`.

```bash
just remote          # prints remote_info + the local ws:// URL + the tailscale serve command
# then open, on the phone:
#   http://<tailnet-host>:7623/?token=<token>
```

Exposure stays loopback-only per ADR-0005: `tailscale serve --bg --https=443
http://127.0.0.1:7623` gives TLS + MagicDNS and keeps the bearer token
meaningful. The token can also be entered in the app (stored in
`localStorage`); passing it in the URL is a pairing convenience — the client
strips it from the address bar and persists it.

## Protocol use

The client speaks exactly [`docs/REMOTE.md`](REMOTE.md):

- JSON text control messages: client→server `list`, `attach`, `input`, `key`,
  `ping`; server→client `sessions`, `caps`, `snapshot`, `pong`, `error`.
- Pane output as **binary frames** using the one canonical 8-byte header
  (`packages/protocol/src/frames.ts`, `FrameType.Output`). The client keeps a
  tail buffer so a frame split across WebSocket messages is reassembled — the
  same rule the daemon-side parser follows for tmux records.

## UX decisions

- **Canonical geometry, scaled to fit (ADR-0002).** The client never resizes the
  pane and never calls `refresh-client -C`. It sets the xterm grid to the `caps`
  cols×rows and CSS-scales the wrapper to the viewport, so the phone and the
  desktop mirror show the *same* grid.
- **Scroll stability (ADR-0004).** The client implements the two-mode state
  machine: an upward scroll sets `anchored` and sends `follow`, streaming output
  never moves the viewport, a persistent **↓ jump-to-bottom** affordance appears
  while anchored, and a snapshot repaint clears only the *visible* screen
  (`reset()` is reserved for the first attach of a session) so scrollback and
  the reading position survive a resync. This is the deliberate fix for the gmux
  "jumps to the top" defect.
- **Session pager.** `list` drives a drawer; tapping attaches. Session switching
  is a **drawer action**, with a narrow left/right **edge strip** as the swipe
  shortcut — a horizontal drag in the terminal body belongs to the running
  program (dooboxes `MOBILE_TERMINAL_UX` §5).
- **Mobile keys.** A Termux-style extra-keys bar (Esc, Tab, sticky Ctrl, arrows,
  C-c, C-d, common punctuation) sits above the soft keyboard. `touch-action:
  pan-y` on the screen so vertical panning reaches xterm's viewport (native
  scrollback) while horizontal drags stay available to the edge strips.
- **Scrollback.** Local xterm scrollback is capped at 1,000 lines (mobile memory
  bound). Server-side scrollback paging is **not** in v1 (`scrollback: false` in
  `caps`), so the phone can scroll what arrived since attach; tmux still holds
  the full history for `ldmux capture --history` (bounded to 5,000 lines).
- **Reconnect.** On socket close the client reconnects with capped exponential
  backoff, re-`list`s and re-`attach`es. A server `error` with code `resync` or
  `too_far_behind` triggers a fresh `attach` (snapshot), so a client that fell
  behind resynchronizes instead of showing a torn grid.

## Installable app (PWA)

The client ships a `manifest.webmanifest` (`display: standalone`,
`orientation: landscape`) and a service worker (`sw.js`) that caches the app
shell. Served over `tailscale serve` the origin is HTTPS, so the phone offers
"Add to home screen" and the app then launches landscape-locked without browser
chrome — the interim mobile app until the native WebView shell exists.

The service worker never touches `/ws` or non-GET requests: terminal traffic is
live and must not be cached. Navigation is network-first (so a redeploy lands)
with the cached shell as an offline fallback; assets are cache-first.

## Android (next)

The WebView host is intentionally unimplemented: `packages/web/public` is
platform-neutral. The Android shell adds landscape lock, a foreground service
holding the WebSocket, and the session pager; the terminal itself stays here.