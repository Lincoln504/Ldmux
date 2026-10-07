# Remote transport

> **Note (2026-10-04, D4).** The remote transport is unchanged in shape (WS on the
> tailnet address, bearer token at the upgrade, ADR-0016). Everything it says
> about how a session is *served* is now the keeper's; there is no tmux server
> behind it.
>
> **Updated 2026-10-06 (branch `fix/audit-hardening`).** The daemon owns the server
> side of the WebSocket frame layer (`crates/daemon/src/ws.rs`) and negotiates
> `permessage-deflate` when the client offers it (ADR-0019); `hello`/`attach` carry
> additive `features`; sessions carry their ending (`exitCode`, `exitSignal`,
> `endedKind`); a local attach socket serves the desk window without the tailnet.
> Each is described below.

How a phone (or any remote client) reaches ldmux. Implements ADR-0016 (direct
tailnet bind) and ADR-0017 (QR pairing) on top of the one canonical frame format
(`crates/protocol/src/frames.rs`) and the keeper-backed pane stream
(`crates/daemon/src/stream.rs`).

## Topology

```
phone ──Tailscale (WireGuard)── ws://<tailnet-address>:7625/ws ──▶ ldmuxd
                                                                  │
                                                         ldmux-keeper
```

- The daemon listens on **its tailnet address or loopback, and nothing else**
  (`LDMUX_REMOTE_HOST`, code default `127.0.0.1`; the deployed machine sets its
  tailnet address in `~/.config/ldmux/env`). Tailnet is `100.64.0.0/10` or
  `fd7a:115c:a1e0::/48`; loopback is `127.0.0.0/8`, `::1` or `localhost`
  (`crates/protocol/src/net.rs`).
- Enforced three times: `ldmuxd` exits on a bad `LDMUX_REMOTE_HOST`,
  the remote server refuses to listen, and a peer outside those ranges gets
  `403` before the token is looked at. Only `/ws` upgrades; any other path is
  `404`.
- Port: `LDMUX_REMOTE_PORT` (code default `7623`; the deployed machine's env file sets `7625`).
- The WebSocket is plain `ws://`; WireGuard encrypts the link. The daemon adds a
  **bearer token**. `tailscale serve` is not used (ADR-0016 supersedes ADR-0005).
- Per-machine values (`LDMUX_REMOTE_HOST`, `LDMUX_REMOTE_PORT`) normally live in the
  optional `~/.config/ldmux/env`, which the unit reads through
  `EnvironmentFile=-%h/.config/ldmux/env` (template: `deploy/ldmux.env.example`).
  With no `LDMUX_REMOTE_HOST` the daemon answers on loopback only.
- At most 64 connections are served at once (`MAX_CONNECTIONS`).
- If the tailnet address does not exist yet at boot, the daemon retries the bind
  (`EADDRNOTAVAIL`) until tailscaled assigns it.

## Wiring it up

```sh
# the daemon runs as the systemd user unit (`nix run .#install`), or: cargo run -p ldmux-daemon --bin ldmuxd
ldmux remote                    # host, port, path, token, and whether it is listening
ldmux code                      # the pairing QR code; scan it from the app's Settings
ldmux pair                      # the same pairing, as an ldmux:// link
```

`code` and `pair` print `ldmux://pair?v=1&host=<addr:port>&token=<64 hex>&name=<machine>`
(`crates/protocol/src/pairing.rs`), and refuse a daemon a phone cannot reach: one on
loopback, or one not listening yet (`remote_info.listening`).

## Authentication

- Token: 32 random bytes (hex) at `$LDMUX_TOKEN_FILE` (the unit sets
  `~/.local/state/ldmux/token`; default `<stateDir>/token`), mode `0600`,
  created on first start, compared in constant time (`crates/daemon/src/auth.rs`).
  A token file, or a state directory, that is not owned by the daemon's uid is refused
  (the daemon will not start on it); a group- or world-accessible token file is
  tightened to `0600`, and the state directory to `0700`.
- Presented as `Authorization: Bearer <token>` (what the app sends) or
  `?token=<token>` (still accepted).
- The Unix socket (local control) is separate and `0600`; `remote_info` exposes
  the token to local callers so `ldmux remote`, `code` and `pair` can read it.

## Local attach socket (no tailnet, no token)

Beside the control socket the daemon binds `<control>-attach.sock` (for the default
path, `ldmuxd-attach.sock`; `protocol::paths`). It speaks the same WebSocket protocol
as `/ws` and ends in the same connection loop, but it is a Unix socket: mode `0600` in
the private state directory, and every connection's peer credentials (`SO_PEERCRED`)
must carry the daemon's uid. The kernel stands in for the bearer token. The desk attach
client (`ldmuxd attach`, `crates/attach`) uses it, so a desk window works with no
tailnet address and no token; only a daemon that predates the socket is reached the old
way, over the remote endpoint that `remote_info` names.

## WebSocket framing and compression

The daemon implements the server side of RFC 6455 itself (`crates/daemon/src/ws.rs`:
handshake, masking, fragmentation, control frames, close codes) because tungstenite can
neither set RSV1 on an outbound frame nor accept it inbound; tungstenite remains the
client library (the attach window, the tests). `permessage-deflate` (RFC 7692) is
negotiated only when the client offers it (OkHttp offers it on every connection):
per-connection context takeover, level 3, `server_max_window_bits` below 15 declined,
messages under 96 bytes sent plain, one WebSocket message = one frame (never fragmented),
and the inbound inflater bounded to the 64 KiB message cap (close code 1009 beyond it).
A client that does not offer the extension gets exactly the uncompressed stream it
always did. Measured ratios (Rust daemon, loopback): `ls -la` 4.2x, `seq 1 100` 2.5x,
`ls -laR` 7.1x, a `yes` flood 691x, an attach snapshot 3.4x; see `docs/WIRE-COST.md` and
ADR-0019.

## Protocol (one connection = one session)

WebSocket at `/ws`. Control/handshake is JSON text; terminal output is binary
frames using the project's single frame format (`FrameType.Output`, 8-byte
header, UTF-8 payload). Open one connection per session you want to view.

Client → server (JSON):

| message | effect |
| --- | --- |
| `{type:"ping",id?}` | liveness; server replies `{type:"pong",id?}` |
| `{type:"hello",device,features?}` | identify the device (id, name, kind); identity, not authentication (ADR-0010). Writing and shell-changing messages need it (see below). `features` is an optional list of strings the client understands: `inplaceResync`, `sessionEnded` |
| `{type:"attach",sessionId,cols?,rows?,takeover?,features?}` | subscribe this connection to a session; a shell held by another device is refused with `owned` unless `takeover`. `features` here has the same meaning as in `hello` |
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
| `{type:"snapshot",resync:true,...}` | an in-place repair of a stale picture, sent (with a fresh `caps`) on the same socket to a client that advertised `inplaceResync` |
| `{type:"session_ended",sessionId,kind,reason?,exitCode?,exitSignal?}` | the session is over and how it ended, sent to a client that advertised `sessionEnded` (see below) |
| `{type:"sessions",sessions:[...]}` | the session list: an answer to `list`, and pushed to **every** client whenever it changes (`new`, `kill`, `close`, a release, a window opening or closing, the maintenance pass) |
| `{type:"created",sessionId,cwd}` | answer to `new`, before its `sessions` |
| `{type:"owned",sessionId,owner}` | the shell is held by another device (attach refused), or was taken from this one |
| `{type:"mousedelivered",col,row}` | the click reached a mouse-aware program |
| `{type:"desktop",inventory}` | answer to `desktop` |
| `{type:"live",sessionId,ok,detail?}` | answer to `makelive` / `closelive` |
| binary `Output` frame | live pane output (UTF-8) |
| `{type:"error",code,message}` | `unknown_session`, `session_dead`, `not_attached`, `bad_json`, `resync`, `resync_failed`, `too_far_behind`, `input_too_large`, `hello_required`, `not_holder`, `input_failed`, `kill_failed`, `close_failed`, `no_mouse`, `shell_foreground`, `bad_cell`, `bad_size`, `resize_failed`, … |

### Additive features

`features` is the only negotiation, and it is additive: a client that says nothing is sent
exactly the messages it always was.

- **`inplaceResync`.** When a client's picture goes stale (output was dropped for a slow
  link, or the keeper discarded unread output), the daemon waits for the link to drain
  and sends a fresh `caps` plus a `snapshot` carrying `"resync": true` on the same
  socket, instead of `error{resync}` and a reconnect. A client without the feature gets
  the legacy `error{code:"resync"}` and re-attaches. The desk window (`ldmuxd attach`)
  advertises it in its first `attach`.
- **`sessionEnded`.** When the session ends, the client is sent `session_ended` (its
  `kind` is one of `exited`, `killed`, `closed`, `keeper_lost`, `window_closed`, or
  `ended` when the record is gone; `reason` is the sentence; `exitCode`/`exitSignal` are
  the shell's own ending) and then released. Attaching to a session that has just ended
  is also served: the final screen as a `snapshot`, then `session_ended` (see "Dead panes"
  below). A client without the feature gets the legacy `resync` / `session_dead`.

### Writes, `hello` and holders

- `input`, `key` and `mouseclick` are the holder's only. A connection attached without a
  `hello` is a read-only observer (the desk mirror, `ldmuxd attach --readonly`): its
  writes are answered `error{hello_required}`; from a device that does not hold the
  shell they are `error{not_holder}`.
- `new`, `kill`, `close`, `makelive` and `closelive` also need a `hello`
  (`hello_required`). `kill` and `makelive` of a shell another device holds are
  `not_holder`. A failure to deliver is `input_failed`, `kill_failed` or `close_failed`.

Six things about `owned` and `sessions` that a client needs:

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
- **Claims survive a daemon restart.** The table is mirrored to `<state>/claims.json` and
  reloaded at start; each restored claim is held for `RESTORE_GRACE_MS` (60 s) for its
  device to reconnect (the phone backs off between retries), and is released when the
  grace ends if nobody re-attaches. Another device cannot slip in between.
- **A claim with no connection behind it is given back** after `CLAIM_GRACE_MS` (3 s),
  and every client hears about it. The claim a shell is taken with on a window's behalf
  (`makelive`, `deskwindow`) is taken *before* the window attaches, so the grace is what a
  connecting window needs. Past it, a claim nothing is behind is a shell no device is
  using and no device can take without pressing `r`: the displaced desk window waits for
  its shell to be *free*, and it never is. The sweep that reaps silent connections reaps
  these too.

### Attach ordering (why output is not lost)

On `attach` the server: **subscribes first** (queueing any output), captures a
snapshot, sends `caps` + `snapshot`, then flushes the queued output and goes
live. A client therefore always receives a full-screen repaint before deltas.

### Backpressure & resync

Live output is best-effort. If the bytes queued for a client's socket pass
`WS_BACKLOG_BYTES` (64 KiB, `crates/daemon/src/remote.rs`) the server sends one
`error{code:"resync"}` and drops output until the client re-attaches and is sent a
fresh snapshot (a client that advertised `inplaceResync` is repaired in place instead;
see "Additive features"). If the keeper stream dies, subscribers get `error{code:"resync"}`
too and the stream reconnects with bounded backoff (`crates/daemon/src/stream.rs`).

Writes (`input`, `key`, `mouseclick`) are the holder's only; see "Writes, `hello` and
holders" above.

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
polls the keeper's `Peek` in bounded chunks (`maxBytes`, 64 KiB; the keeper answers
`data64`, base64, with `more` set while it holds further bytes, so a flood is read in
pieces rather than as one huge JSON byte array), fans the bytes out as binary `OUTPUT` frames
(coalesced at 8 ms / 4 KiB), and a snapshot taken for an attach is cut at the
same byte boundary as the live stream so no output is replayed or lost. There
are no stream modes.

A keeper that predates bounded peeks ignores `maxBytes` and answers the whole buffer as a
byte array; the daemon reads either form, and logs once (to the journal) that the running
keeper predates the compact form. The keeper is replaced only by stopping it, which ends
every shell it owns (`nix run .#restart-keeper`), so a flood is served worse until that is
done at a convenient moment.

### Dead panes

A shell that has exited keeps its pane in the keeper for 60 s (`DEAD_KEEP_MS`) after the
daemon first sees it dead, so a client that reconnects in that window can read the final
screen and be told how it ended (a client that advertised `sessionEnded`); only then is
the pane released. (The keeper's own backstop, for a keeper whose daemon is away, is 15
minutes.) A client without the feature is refused with `session_dead` as before.

## Session listing

`{type:"list"}` answers with `sessions[]`, each carrying `id`, `kind`, `title`
(derived from the working directory when the record only has a default title),
`cwd`, `status`, `keeperName`, `endedReason`, and, once it has ended, `exitCode`
(the shell's own status), `exitSignal` (the signal that killed it) and `endedKind`
(`exited | killed | closed | keeper_lost | window_closed`; the same fact as
`endedReason`, in a closed set), and `lastActivityAt` (epoch ms, the keeper's own
per-session activity clock) so a client can show an unread marker.
Listing is **read-only**: it reconciles without sweeping streams.

## Deliberately out of scope (v1)

- Unbounded scrollback: paging is bounded to 5,000 lines per `scrollback`
  request and the local client buffer to 1,000 lines.
- Multiplexing many sessions on one socket (open N sockets).