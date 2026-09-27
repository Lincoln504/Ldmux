# Remote transport

How a phone (or any remote client) reaches ldmux. Implements ADR-0016 (direct
tailnet bind) and ADR-0017 (QR pairing) on top of the one canonical frame format
(`frames.ts`) and the control-mode stream (`stream.ts`).

## Topology

```
phone ──Tailscale (WireGuard)── ws://100.127.119.54:7625/ws ──▶ ldmuxd
                                                                  │
                                                        tmux -L ldmux
```

- The daemon listens on **its tailnet address or loopback, and nothing else**
  (`LDMUX_REMOTE_HOST`, code default `127.0.0.1`; the deployed unit sets this
  machine's tailnet address). Tailnet is `100.64.0.0/10` or
  `fd7a:115c:a1e0::/48`; loopback is `127.0.0.0/8`, `::1` or `localhost`
  (`protocol/src/net.ts`).
- Enforced three times: `main.ts` exits on a bad `LDMUX_REMOTE_HOST`,
  `RemoteServer.start` refuses to listen, and a peer outside those ranges gets
  `403` before the token is looked at. Only `/ws` upgrades; any other path is
  `404`.
- Port: `LDMUX_REMOTE_PORT` (code default `7623`; the deployed unit uses `7625`).
- The WebSocket is plain `ws://`; WireGuard encrypts the link. The daemon adds a
  **bearer token**. `tailscale serve` is not used (ADR-0016 supersedes ADR-0005).
- If the tailnet address does not exist yet at boot, the daemon retries the bind
  (`EADDRNOTAVAIL`) until tailscaled assigns it.

## Wiring it up

```sh
# the daemon runs as the systemd user unit (`just install`), or: just daemon
just remote                     # host, port, path, token, and whether it is listening
just code                       # the pairing QR code; scan it from the app's Settings
just pair                       # the same pairing, as an ldmux:// link
```

`code` and `pair` print `ldmux://pair?v=1&host=<addr:port>&token=<64 hex>&name=<machine>`
(`protocol/src/pairing.ts`), and refuse a daemon a phone cannot reach: one on
loopback, or one not listening yet (`remote_info.listening`).

## Authentication

- Token: 32 random bytes (hex) at `$LDMUX_TOKEN_FILE` (the unit sets
  `~/.local/state/ldmux/token`; default `<stateDir>/token`), mode `0600`,
  created on first start, compared in constant time (`auth.ts`).
- Presented as `Authorization: Bearer <token>` (what the app sends) or
  `?token=<token>` (still accepted).
- The Unix socket (local control) is separate and `0600`; `remote_info` exposes
  the token to local callers so `ldmux remote`, `code` and `pair` can read it.

## Protocol (one connection = one session)

WebSocket at `/ws`. Control/handshake is JSON text; terminal output is binary
frames using the project's single frame format (`FrameType.Output`, 8-byte
header, UTF-8 payload). Open one connection per session you want to view.

Client → server (JSON):

| message | effect |
| --- | --- |
| `{type:"ping",id?}` | liveness; server replies `{type:"pong",id?}` |
| `{type:"hello",device}` | identify the device (id, name, kind); identity, not authentication (ADR-0010) |
| `{type:"attach",sessionId,cols?,rows?,takeover?}` | subscribe this connection to a session; a shell held by another device is refused with `owned` unless `takeover` |
| `{type:"resize",cols,rows}` | the client's grid changed; tmux is resized and a fresh snapshot follows |
| `{type:"mouseclick",col,row}` | a tap in a pane whose program enabled SGR mouse reporting |
| `{type:"copymode",on}` | enter or leave tmux copy-mode (scrolling the alternate screen) |
| `{type:"input",data}` | type literal text into the pane |
| `{type:"key",keys:["Enter"]}` | send named keys |
| `{type:"follow",mode}` | scroll intent; **client-owned** (ADR-0004) |
| `{type:"list"}` | list sessions (id, kind, title, cwd, status, activity) |
| `{type:"new",kind?,cwd?,title?}` | open a session on the host (FR-10); answers with a fresh `sessions` |
| `{type:"kill",sessionId}` | end a session; answers with a fresh `sessions` |
| `{type:"close",sessionId}` | the user closed a shell here; the daemon decides whether that ends it (ADR-0010) |
| `{type:"desktop"}` | the machine's desktop inventory (workspaces, windows) |
| `{type:"makelive",sessionId,workspace}` | put a shell on the desktop, on that workspace; answers with `live` |
| `{type:"closelive",sessionId}` | take a shell's desktop window away; answers with `live` |
| `{type:"scrollback",sessionId,lines?}` | re-send the pane **including paged history** |

Server → client:

| message | meaning |
| --- | --- |
| `{type:"caps",...}` | negotiated geometry, `snapshotEpoch`, capabilities |
| `{type:"snapshot",...}` | full visible screen + terminal state (repaint from this) |
| `{type:"snapshot",history:true,...}` | answer to `scrollback`: the screen includes older lines |
| `{type:"sessions",sessions:[...]}` | answer to `list`, `new`, `kill`, `close` |
| `{type:"created",sessionId,cwd}` | answer to `new`, before its `sessions` |
| `{type:"owned",sessionId,owner}` | the shell is held by another device (attach refused), or was taken from this one |
| `{type:"mousedelivered",col,row}` | the click reached a mouse-aware program |
| `{type:"desktop",inventory}` | answer to `desktop` |
| `{type:"live",sessionId,ok,detail?}` | answer to `makelive` / `closelive` |
| binary `Output` frame | live pane output (UTF-8) |
| `{type:"error",code,message}` | `unknown_session`, `session_dead`, `not_attached`, `bad_json`, `resync`, `too_far_behind`, `input_too_large`, `not_holder`, `no_mouse`, `shell_foreground`, `bad_cell`, `bad_size`, `resize_failed`, … |

### Attach ordering (why output is not lost)

On `attach` the server: **subscribes first** (queueing any output), captures a
snapshot, sends `caps` + `snapshot`, then flushes the queued output and goes
live. A client therefore always receives a full-screen repaint before deltas.

### Backpressure & resync

Live output is best-effort. If a client's socket buffer exceeds
`maxBufferedBytes` (4 MiB) the server emits `error{code:"too_far_behind"}` and
closes; the client reconnects and re-attaches, receiving a fresh snapshot. If
the control stream dies, subscribers get `error{code:"resync"}` and the stream
reconnects with bounded backoff (`stream.ts`).

Writes (`input`, `key`, `mouseclick`, `copymode`) are the holder's only; from
any other device they get `error{code:"not_holder"}`.

Inbound payloads are capped: 16 KiB of `input` data (a typed paste), 32 keys per
`key` message, and 64 KiB per WebSocket frame.

### Paused panes (`%pause`)

tmux pauses a pane whose output is not being consumed (`refresh-client -f
pause-after=2`). A paused pane's **queued output is discarded**, so the server
treats `%pause` as a resync point: subscribers receive `error{code:"resync"}`
(throttled to once a second) and the client re-attaches for a fresh snapshot.
The server then resumes the pane with `refresh-client -A <pane>:continue`
(throttled per pane).

### Stream modes

A stream is in one of two modes:

| mode | how output is produced | when |
| --- | --- | --- |
| `control` | `tmux -C` notifications (lowest latency, exact bytes) | normally |
| `polling` | `capture-pane` at ~250 ms, diffed row-wise | when control mode cannot be established (3 failed reconnects) |

Polling is a fallback, not a peer: it has no VT model, so it rewrites changed
rows in place and repaints only when the row count changes. Control mode is
retried every 30 s while polling. The selected mode is internal — the wire
protocol is identical either way.

## Session listing

`{type:"list"}` answers with `sessions[]`, each carrying `id`, `kind`, `title`
(derived from the working directory when the record only has a default title),
`cwd`, `status`, `tmuxName`, and `lastActivityAt` (epoch ms, from tmux
`session_activity` for terminal sessions) so a client can show an unread marker.
Listing is **read-only**: it reconciles without sweeping streams.

## Deliberately out of scope (v1)

- Unbounded scrollback: paging is bounded to 5,000 lines per `scrollback`
  request and the local client buffer to 1,000 lines.
- Multiplexing many sessions on one socket (open N sockets).