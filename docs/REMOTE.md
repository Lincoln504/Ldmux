# Remote transport

How a phone (or any remote client) reaches ldmux. Implements ADR-0005
(loopback + Tailscale) on top of the one canonical frame format (`frames.ts`)
and the control-mode stream (`stream.ts`).

## Topology

```
phone ──Tailscale (WireGuard, TLS via `tailscale serve`)──▶ 127.0.0.1:<port> ──▶ ldmuxd
                                                                                │
                                                                      tmux -L ldmux
```

- The daemon binds **loopback only** (`LDMUX_REMOTE_HOST`, default `127.0.0.1`).
  It never binds a public interface.
- A fixed port (`LDMUX_REMOTE_PORT`, default `7623`) lets `tailscale serve` be
  configured once.
- Tailscale terminates TLS and gives MagicDNS; the daemon adds a **bearer token**
  (defence in depth, and the only gate if the port is ever forwarded).

## Wiring it up

```sh
# daemon is already running (the pi extension ensures it, or: just daemon)
just cli remote                 # prints host, port, token, and the serve command
tailscale serve --bg --https=443 http://127.0.0.1:7623
```

Clients then connect to `wss://<machine>.<tailnet>.ts.net/ws`.

`tailscale serve` also injects `Tailscale-User-Login`; the bearer token remains
the authoritative gate so the daemon does not depend on proxy headers.

## Authentication

- Token: 32 random bytes (hex) at `<stateDir>/token`, mode `0600`, created on
  first start, compared in constant time (`auth.ts`).
- Presented as either `Authorization: Bearer <token>` or `?token=<token>`.
- The Unix socket (local control) is separate and `0600`; `remote_info` exposes
  the token to local callers so `just cli remote` and pairing can read it.

## Protocol (one connection = one session)

WebSocket at `/ws`. Control/handshake is JSON text; terminal output is binary
frames using the project's single frame format (`FrameType.Output`, 8-byte
header, UTF-8 payload). Open one connection per session you want to view.

Client → server (JSON):

| message | effect |
| --- | --- |
| `{type:"ping",id?}` | liveness; server replies `{type:"pong",id?}` |
| `{type:"attach",sessionId}` | subscribe this connection to a session |
| `{type:"input",data}` | type literal text into the pane |
| `{type:"key",keys:["Enter"]}` | send named keys |
| `{type:"follow",mode}` | scroll intent; **client-owned** (ADR-0004) |
| `{type:"list"}` | list sessions (id, kind, title, cwd, status, activity) |
| `{type:"scrollback",sessionId,lines?}` | re-send the pane **including paged history** |

Server → client:

| message | meaning |
| --- | --- |
| `{type:"caps",...}` | negotiated geometry, `snapshotEpoch`, capabilities |
| `{type:"snapshot",...}` | full visible screen + terminal state (repaint from this) |
| `{type:"snapshot",history:true,...}` | answer to `scrollback`: the screen includes older lines |
| `{type:"sessions",sessions:[...]}` | answer to `list` |
| binary `Output` frame | live pane output (UTF-8) |
| `{type:"error",code,message}` | `unknown_session`, `session_dead`, `not_attached`, `bad_json`, `resync`, `too_far_behind`, `input_too_large`, … |

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

- Per-client resizing — geometry is canonical and pinned (ADR-0002); a phone
  renders a scaled/panned viewport over the canonical grid.
- Unbounded scrollback: paging is bounded to 5,000 lines per `scrollback`
  request and the local client buffer to 1,000 lines.
- Multiplexing many sessions on one socket (open N sockets).