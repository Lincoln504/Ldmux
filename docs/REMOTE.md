# Remote transport

> **Note (2026-10-04, D4).** The remote transport is unchanged in shape (WS on the
> tailnet address, bearer token at the upgrade, ADR-0016). Everything it says
> about how a session is *served* is now the keeper's; there is no tmux server
> behind it.


How a phone (or any remote client) reaches ldmux. Implements ADR-0016 (direct
tailnet bind) and ADR-0017 (QR pairing) on top of the one canonical frame format
(`crates/protocol/src/frames.rs`) and the keeper-backed pane stream
(`crates/daemon/src/stream.rs`).

## Topology

```
phone ──Tailscale (WireGuard)── ws://100.127.119.54:7625/ws ──▶ ldmuxd
                                                                  │
                                                         ldmux-keeper
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
| `{type:"resize",cols,rows}` | the client's grid changed; the session's pty is resized and a fresh snapshot follows |
| `{type:"mouseclick",col,row}` | a tap in a pane whose program enabled SGR mouse reporting |
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
| `{type:"sessions",sessions:[...]}` | the session list: an answer to `list`, and pushed to **every** client whenever it changes (`new`, `kill`, `close`, a release, a window opening or closing, the maintenance pass) |
| `{type:"created",sessionId,cwd}` | answer to `new`, before its `sessions` |
| `{type:"owned",sessionId,owner}` | the shell is held by another device (attach refused), or was taken from this one |
| `{type:"mousedelivered",col,row}` | the click reached a mouse-aware program |
| `{type:"desktop",inventory}` | answer to `desktop` |
| `{type:"live",sessionId,ok,detail?}` | answer to `makelive` / `closelive` |
| binary `Output` frame | live pane output (UTF-8) |

Four things about `owned` and `sessions` that a client needs:

- **A shell can have no owner.** `sessions` carries each shell's owner, so a client
  learns a shell was let go without having asked for anything. The desk attach client
  uses exactly that: it re-attaches by itself when its session is running and *nobody*
  holds it, because a device that puts a shell down is not using it. A shell another
  device still holds keeps waiting for the reclaim key (`r`).
- **No running copy of its shell in the list means the shell is over.** A displaced
  window is unsubscribed, so it sees neither the pane's exit nor the session's death;
  the attach client re-attaches on that and lets the daemon's verdict —
  `error{session_dead}` — say "This shell has ended." and end the window. A notice is
  only ever replaced by an answer, so a client that stops asking keeps the notice for
  good.
- **`owned` is sent on a refusal *and* on a seizure.** `owner` names the holder when
  the attach was refused; it is `null` on the notice to a device that was displaced,
  whose connection the daemon then unsubscribes (it stays connected and idle).
- **A claim goes with the shell being watched.** Attaching a connection to another
  shell releases the claim it held on the previous one, unless another connection of
  the same device is still on it. `close` from a phone on a shell the desk started is a
  *release*, not an end: the shell keeps running and is broadcast as unowned.
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

Writes (`input`, `key`, `mouseclick`) are the holder's only; from
any other device they get `error{code:"not_holder"}`.

Inbound payloads are capped: 16 KiB of `input` data (a typed paste), 32 keys per
`key` message, and 64 KiB per WebSocket frame.

### Backpressure and resync

The keeper always reads the pty (the kernel buffer never fills) and holds at
most 4 MiB of unread output per session; if a session is unwatched past that,
the oldest output is dropped and the next poll reports it as lost. A connection
that is more than 64 KiB behind the link is not buffered for: subscribers
receive `error{code:"resync"}` once per backlog and the client re-attaches for a
fresh snapshot. A subscriber is never silenced for the volume of output alone.

### Streams

A stream is one keeper session viewed by any number of connections: the daemon
polls the keeper's `Peek`, fans the bytes out as binary `OUTPUT` frames
(coalesced at 8 ms / 4 KiB), and a snapshot taken for an attach is cut at the
same byte boundary as the live stream so no output is replayed or lost. There
are no stream modes.

## Session listing

`{type:"list"}` answers with `sessions[]`, each carrying `id`, `kind`, `title`
(derived from the working directory when the record only has a default title),
`cwd`, `status`, `keeperName`, and `lastActivityAt` (epoch ms, the keeper's own
per-session activity clock) so a client can show an unread marker.
Listing is **read-only**: it reconciles without sweeping streams.

## Deliberately out of scope (v1)

- Unbounded scrollback: paging is bounded to 5,000 lines per `scrollback`
  request and the local client buffer to 1,000 lines.
- Multiplexing many sessions on one socket (open N sockets).