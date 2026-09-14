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

Server → client:

| message | meaning |
| --- | --- |
| `{type:"caps",...}` | negotiated geometry, `snapshotEpoch`, capabilities |
| `{type:"snapshot",...}` | full visible screen + terminal state (repaint from this) |
| binary `Output` frame | live pane output (UTF-8) |
| `{type:"error",code,message}` | `unknown_session`, `bad_json`, `resync`, `too_far_behind`, … |

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

## Deliberately out of scope (v1)

- Per-client resizing — geometry is canonical and pinned (ADR-0002); a phone
  renders a scaled/panned viewport over the canonical grid.
- Server-side scrollback paging beyond the snapshot (`Scrollback` frame type is
  reserved).
- Multiplexing many sessions on one socket (open N sockets).