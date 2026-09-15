# ldmux web client

`packages/web/public` is the **reference terminal client** — a buildless static
bundle (HTML + CSS + vendored xterm.js) served by the daemon itself. On a phone
it is the whole product today; on Android it becomes the payload of the
`TerminalSurface` WebView seam, so the rendering code is written once.

There is **no build step and no runtime dependency**: xterm.js 6.0.0 is vendored
as UMD files under `public/vendor/` (license included). `@xterm/addon-fit` is
deliberately *not* vendored: fitting the terminal would reflow the canonical grid
(ADR-0002), so scaling is done in CSS on the wrapper instead. This is consistent
with the repo-wide "buildless" rule; the only third-party code is a static asset
the daemon serves.

## Layout and testing

| File | Role |
|------|------|
| `public/index.html`, `public/app.css` | shell + layout (viewport, drawer, key bar) |
| `public/core.js` | **DOM-free core**: frame decoder, scroll state machine, snapshot-repaint math, `escapeHtml`, token parsing. Attaches one global, `LdmuxCore`. |
| `public/app.js` | the client: xterm wiring, WebSocket, drawer, key bar, scaling |
| `public/sw.js`, `public/manifest.webmanifest`, `public/icon.svg` | installable app shell |

`core.js` exists so the protocol-critical logic is unit-testable without a
browser: `packages/web/test/core.test.ts` runs it under `node --test` (frame
reassembly across chunks, the poisoned-length DoS guard, the scroll/anchor
state machine, 1-based cursor restore, escaping, token precedence) and asserts
`index.html` loads `core.js` before `app.js`.

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

### Testing on a real phone (USB, no tailnet needed)

When the phone is on the tailnet, use the URL above. When it is plugged in by
USB instead, forward the daemon's loopback port to the phone with `adb reverse`
— this exercises the exact same client over the same WebSocket, with no
`tailscale serve` and no firewall changes:

```bash
adb reverse tcp:7629 tcp:7629          # phone's localhost:7629 -> this machine's 127.0.0.1:7629
# ...start a daemon with LDMUX_REMOTE_PORT=7629, create a session...
adb shell am start -a android.intent.action.VIEW -d "http://localhost:7629/?token=<token>"
```

Verified against a real device on 2026-09-13 (Motorola Edge 2024, Chrome): the
PWA loads, the status shows **online** (the WebSocket connected), the drawer
lists sessions with cwd-derived titles and unread dots, attaching streams live
pane output, the `⤒` scrollback control is present (caps advertised
`scrollback: true`), the extra-keys bar's buttons send input (a tapped `|`
appeared in the pane), and tapping the terminal focuses it and raises the IME
(`adb shell input text` then reached the pane). Note that `adb shell input
keyevent`/`input text` only work once the terminal has been tapped — the tap is
what focuses xterm.js's hidden textarea.

## Protocol use

The client speaks exactly [`docs/REMOTE.md`](REMOTE.md):

- JSON text control messages: client→server `list`, `attach`, `input`, `key`,
  `follow`, `scrollback`, `ping`; server→client `sessions`, `caps`, `snapshot`
  (with `history: true` for a paged answer), `pong`, `error`.
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
  C-c, C-d, common punctuation) sits above the soft keyboard, with a `≡` page for
  symbols. Every button is at least 44×44 px and glyph buttons carry an
  `aria-label`. `touch-action: pan-y` on the screen so vertical panning reaches
  xterm's viewport (native scrollback) while horizontal drags stay available to
  the edge strips.
- **Scrollback.** Local xterm scrollback is capped at 1,000 lines (mobile memory
  bound). Older history is fetched on demand: the `⤒` button (shown when `caps`
  advertises `scrollback`) sends a `scrollback` request and the server answers
  with a snapshot carrying up to 5,000 lines of history, which the client
  repaints from scratch so the older lines land in scrollback.
- **Unread markers.** The session list carries `lastActivityAt`; the client
  remembers the timestamp it last looked at per session (`localStorage`) and
  shows a dot for anything newer.
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
with the cached shell as an offline fallback; assets are
**stale-while-revalidate** (serve the cached copy, refresh it in the background),
because cache-first pinned clients to a stale `app.js` after a deploy.

## Android

`packages/web/public` stays platform-neutral: the native KMP/Compose shell in
`packages/mobile` hosts it in a `WebView` (the `TerminalSurface` seam), so the
terminal is the client below and there is still exactly one terminal engine
(ADR-0006). The app adds landscape lock, a thin top bar, and a `HorizontalPager`
that puts one session per page — **swipe between shells** (FR-8) — with the
session list fetched over the daemon's WebSocket (`list`/`new`/`kill`). A
foreground service (so the heartbeat survives Doze) is still to come; see
`packages/mobile/README.md` for the build.