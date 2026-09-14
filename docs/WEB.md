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
- **Scroll stability (ADR-0004).** Output is written with
  `Terminal.write()`, which appends; the viewport only moves on explicit user
  scroll. This is the deliberate fix for the gmux "jumps to the top" defect.
- **Session pager.** `list` drives a drawer; tapping attaches, horizontal swipe
  steps to the next/previous attachable session (RPC-only `pi` records with an
  empty `tmuxName` are shown but not attachable).
- **Mobile keys.** A Termux-style extra-keys bar (Esc, Tab, sticky Ctrl, arrows,
  C-c, C-d, common punctuation) sits above the soft keyboard; `touch-action:
  none` on the screen prevents browser scroll/zoom fighting the terminal.
- **Reconnect.** On socket close the client reconnects with capped exponential
  backoff, re-`list`s and re-`attach`es. A server `error` with code `resync` or
  `too_far_behind` triggers a fresh `attach` (snapshot), so a client that fell
  behind resynchronizes instead of showing a torn grid.

## Android (next)

The WebView host is intentionally unimplemented: `packages/web/public` is
platform-neutral. The Android shell adds landscape lock, a foreground service
holding the WebSocket, and the session pager; the terminal itself stays here.