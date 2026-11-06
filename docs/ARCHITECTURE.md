# ldmux architecture

One phone talks to one Linux machine. The machine owns the shells. The phone is a
viewer and an input device, and it is a full one: it draws the terminal itself,
decodes the wire, and decides the pane size.

## 1. Components

| Component | Binary or module | Job |
|---|---|---|
| daemon | `crates/daemon`, `ldmuxd` | owns the session registry, the control socket, the tailnet WebSocket, the desk-window attach socket, ownership, and the mesh link |
| keeper | `crates/keeper`, `ldmux-keeper` | owns every PTY and one libghostty-vt per session, survives a daemon restart, serves `Spawn`/`Kill`/`Write`/`Resize`/`Read`/`Peek`/`Snapshot`/`List`/`Close` on a Unix socket |
| CLI | `crates/daemon`, `ldmux` | every verb the daemon serves: `list`, `new`, `send`, `key`, `capture`, `window`, `live`, `mirror`, `kill`, `close`, `code`, `pair`, `mesh` |
| desk window | `crates/attach`, `ldmuxd attach` | a terminal window on the machine's own screen, attached over the daemon's local attach socket, no tailnet involved |
| phone | `packages/mobile` | Kotlin Multiplatform and Compose, its own VT emulator over libghostty-vt behind JNI, the wire clients for both routes |
| serial carrier | `crates/companion` | owns the host's USB radio and speaks the MeshCore companion protocol |
| mesh link | `crates/mesh`, `crates/session`, `crates/session-ffi` | L2 frame, L3 Noise session with ARQ, and the same session as a C ABI for the phone |
| terminal protocol | `crates/protocol/src/mesh_term.rs`, `packages/mobile/.../mesh/MeshTerm.kt` | the screen protocol the phone and the host speak over the L3 session |

The keeper is the source of truth. The daemon's registry is a small JSON cache of
metadata the keeper cannot hold. A daemon restart keeps every shell. Replacing the
keeper ends every shell it holds, which is why `scripts/restart-keeper.sh` says so
before it does anything.

## 2. The three modes

| Mode | Carriage | What the user gets |
|---|---|---|
| tailnet | a WebSocket to the daemon's tailnet address, bearer token at the upgrade | the full terminal: predictive echo, resync instead of reconnect, a shared pane with a desk window |
| LoRa live | two MeshCore radios, one on the host's serial port, one on the phone's Bluetooth LE | keystrokes leave as they are typed, so typing is direct and every key costs airtime |
| LoRa batch | the same radios | typed text is collected on the phone and leaves the radio as one batch when the user presses `sync`, which is forced on a public channel |

Batch is the mode name in the app, the docs and the settings row (`input: live |
batch`). The identifier in the code is still `MeshInputMode.ManualSync`.

**The batch view is frozen on purpose, and the one control carries both verbs.** It
reads `sync` when text is waiting (send it) and `show` when only screens are held (draw
the newest). A press that sends keeps the view open while the answer arrives: every
screen that lands pushes the deadline out by 900 ms, so the view follows the answer
until the shell goes quiet and then freezes again, with an 8 s ceiling so a build log
cannot hold it open. Leaving the shell drops both directions of that state — the typing
and the held screens — because they belong to the page, not to the link. The bar meters
what a press costs in records before it is pressed (`about N records`); a long paste is
split across records by the pump, and the queue limit is a memory bound, not an airtime
one.

Exclusivity is per shell, not per link: the host accepts up to 64 connections, and a
public channel is classified from the tuple the radio reports, never from a configured
name. `new shell` is the first row of the list on both routes, because it is the one
action the list always offers.

### The desk surface

**A desk window is the terminal already open.** `ldmux attach <id>` runs the same client a
desk window runs, in the terminal it is started in; `ldmux shell` creates a session and becomes
its window there. The CLI opens nothing: `ldmux new` creates a shell and
prints its id so a script can carry on.

The one window the daemon spawns is the one asked for by a device that has no terminal — the
phone's `send to desktop`. It opens in **the terminal the person configured**, resolved in this
order (probes cached per daemon process, a pin read live):

1. `ldmux terminal set <program>` — this user's explicit choice, kept in
   `$XDG_CONFIG_HOME/ldmux/terminal`. A file because the daemon runs from the user manager:
   `$TERMINAL` exported in a shell never reaches it.
2. `$TERMINAL`.
3. `xdg-terminal-exec`, when installed: it reads the XDG terminal configuration and every
   desktop application goes through it.
4. **This session's own desktop setting**, read only from that desktop — GNOME/Cinnamon/MATE
   through their `gsettings` schema, KDE through `kdeglobals`, XFCE through `xfconf` and
   `~/.config/xfce4/helpers.rc`. A bare window manager has no such key and is asked nothing.
5. An emulator already running for this user, most frequent first — what makes a bare i3
   session resolve to reality.
6. The system alternative `x-terminal-emulator`, resolved through its symlink and its
   `.wrapper` script to the program it actually runs, so that program's template still applies.
7. The known emulators on `PATH`.

Every answer that is an indirection is followed: a probe that returns `x-terminal-emulator`, a
`.wrapper` shim or a `.desktop` id is not a terminal, and returning it raw is how a window
opens in something nobody named. `ldmux terminal` prints the program, `--explain` prints the
step it came from, and `--exec <command>` applies that emulator's own template and execs it,
so the keybinding wrapper carries no second copy of the rules. The pin is read on every
resolution, so `ldmux terminal set` applies without a restart; the desktop probes are cached
per daemon process, so a change to the desktop's own setting applies on the next start.

Each spawned window is a transient unit under `ldmux-windows.slice`, held on a pipe until the
move is visible in `/proc/<pid>/cgroup` (the same construction the keeper uses for shells), so
a window's processes are not charged to the daemon's cgroup and are gone with it.

## 3. Layers

- L1, the radios. A MeshCore companion radio on each end. The presets must match:
  frequency, bandwidth, spreading factor and coding rate.
- L2, the carriage. The daemon owns the host's radio over USB serial and speaks the
  MeshCore companion protocol (`crates/companion`, driven by
  `crates/daemon/src/mesh_serial.rs`); the phone owns its own radio over Bluetooth LE.
  `crates/mesh` is the vocabulary both sides of the session loop share — the carrier
  seam, the record limits and the radio state — and holds no radio knowledge.
- L3, the session. A Noise session with an ARQ, one record in flight at a time,
  acknowledgements, retransmission and a replay window. `crates/session`.
- L4, the terminal. `MeshTerm`: profiles, rows, commits, keys, resizes, and the shell
  control messages.
- Above that, the bridge. `crates/daemon/src/mesh_bridge.rs` turns the keeper's
  screen into rows and the phone's input into keystrokes, and it is the only place
  that decides what a phone is allowed to do to a shell.

The tailnet route has the same shape without the radios: JSON RPC on the control
socket, a WebSocket per viewed session, binary output frames, and snapshots.

## 4. The wire

The tailnet route:

- one WebSocket per attached session at `WS /ws` on the daemon's tailnet address
- bearer token checked at the upgrade in the `Authorization` header, scheme
  case-insensitive. **The query string is not a credential**: a URL reaches proxy logs,
  crash reports and history in ways a header does not, and no client sends one
- a plain GET answers 401 for a refused token and 400 for an accepted token whose
  upgrade was malformed, which is how the phone tells "wrong password" from
  "machine off"
- JSON control messages and binary output frames, `permessage-deflate` when offered
- one attach per connection, ownership checked before any write

Mesh terminal protocol, version 9:

| tag | direction | message |
|---|---|---|
| 0x01 | phone to host | `Attach` version, selector |
| 0x02 | phone to host | `Input` |
| 0x03 | phone to host | `Resync` |
| 0x04 | phone to host | `Detach` |
| 0x05 | phone to host | `List` |
| 0x06 | phone to host | `Key` named keys |
| 0x07 | phone to host | `Resize` |
| 0x08 | phone to host | `Create` version, cwd |
| 0x09 | phone to host | `Close` selector |
| 0x0a | phone to host | `MakeLive` selector |
| 0x0b | phone to host | `Takeover` selector |
| 0x81 | host to phone | `Profile` version, cols, rows, unsupported, shell |
| 0x82 | host to phone | `Rows` epoch, full, patches |
| 0x83 | host to phone | `Commit` epoch, cursor, flags, first line |
| 0x84 | host to phone | `Closed` reason |
| 0x85 | host to phone | `Sessions` pages with flags for cwd, origin and holder |
| 0x86 | host to phone | `Created` id |
| 0x87 | host to phone | `Marks` shell id and kind, paged to fit a message |

Version 7 added shell control. Version 8 added the holder and the takeover. Version 9 added the
mark table: one entry per marked shell, paged because a message's body is capped at 134 bytes and a
dozen marks overflow one, and sent with the `Sessions` answer and with an `Attach` rather than
pushed, because the radio cannot reach a phone that is not connected.

A `sessions` entry carries who started the shell and who holds it now, each as one byte from a
fixed vocabulary, because two length-prefixed words made the worst case one byte wider than a record
has to carry.

`Commit.flags`: bit 0 alternate screen, bit 1 the shell has exited, bit 2 **idle** — the pane has
stopped changing and this commit carries no rows. The idle bit is additive (unknown bits are
ignored), it is sent once per burst about 700 ms after the last row, and it exists so the phone
can end its wait for an answer at the moment the answer ends instead of on a quiet timer. A host
that never sends it costs the phone one record of latency, not correctness.

`Closed` reasons: not found, ambiguous, exited, unavailable, version, held.

The phone paints on `Commit` only, so a screen is never half drawn. A record holds
at most 143 application bytes after the ARQ header, and every host to phone message
is built to fit one.

## 5. Invariants

1. The keeper owns the PTYs and outlives the daemon. Nothing else allocates a
   terminal.
2. One canonical pane size. The viewer decides it, the daemon clamps it, and a
   client that cannot measure its terminal omits the size rather than inventing one.
3. A shell has at most one holder, and only the holder may write. Ownership lives in
   memory, keyed by connection, never in a record that can outlive it.
4. A shell another device holds is refused, not stolen. The tailnet answers
   `owned`, the radio answers `Closed { HELD }`, and both offer a takeover the user
   makes.
5. Every host to phone message fits one record. `MAX_MESSAGE` is derived from
   `MAX_ROWS_BODY` and checked against the record ceiling by a test.
6. The ciphertext is the boundary. Everything above L3, including the terminal
   protocol, is inside the sealed record, on a public channel as much as a private
   one.
7. State that cannot be rebuilt is not persisted. Claims, cursors and in-flight
   records are per connection and die with it.
8. Nothing is sent into a queue that cannot report how much it holds. The outbound
   meter reads the ARQ's own queued bytes.
9. An idle screen costs nothing. The differ records everything it told the phone —
   rows, cursor, flags **and the absolute top line** — so a screen that has not changed
   produces no message. A field left out of that record makes `up_to_date` false for
   ever and spends a record a tick on nothing (measured 2026-11-06: one `Commit` a
   second on an untouched shell).
10. Per-shell state lives on that shell's page. The launcher shows shells, never what
    one of them has typed or has not read.

## 6. Ownership

ADR-0010 is the decision. The practice:

- A device claims a shell by attaching. The daemon records the claim against the
  connection that made it.
- The daemon tells a displaced viewer `owned`, cuts its stream, and the viewer
  prints `in use on X, press r to take it back`. Pressing `r` is the consent.
- A claim nobody is watching is released after a short grace, because a claim taken
  on a window's behalf must not outlive the window. A phone over the radio has no
  connection in that table, so the bridge tells the daemon which sessions the mesh
  link is watching and those claims are kept.
- When the holder lets go, the daemon broadcasts the session list and a displaced
  desk window takes the shell back by itself, including its own pane size.
- **Limits, stated rather than implied.** A mesh claim is kept while the link watches the
  shell, and the radio record carries no heartbeat, so a phone that disappears without a
  `Detach` keeps its claim until the link itself is rebuilt or the daemon restarts. A mesh
  viewer is also not told when a desk window takes its shell: the refusal is enforced on
  `Attach` and on the write path (a non-holder `Input`/`Key` is refused as `HELD`), but the
  phone learns it when its next write is refused, not when the takeover happens. A
  heartbeat sized for airtime is the fix for both, and it is open work.

## 7. Security

- The pairing code carries the daemon token for the tailnet route, and for the radio
  route the session PSK, the host radio public key and the pairing secret. A code
  made with `--mesh` is as sensitive as the token and the secret together, and it is
  printed to a terminal, so shell history and scrollback are inside the trust
  boundary.
- The mesh session is `Noise_IKpsk2_25519_ChaChaPoly_SHA256` over `snow`. The record
  is 6 clear bytes plus 2 inner bytes plus a 16-byte tag. The 32-bit record counter
  is the AEAD nonce. The tag is never truncated, because truncation turns failed
  authentication into an oracle.
- The session gives forward secrecy per session from ephemeral keys that are
  zeroized on drop. It gives no post-compromise security, because there is no
  ratchet.
- The host pins the phone's static key on the first accepted handshake.
- Host secrets are files at 0600 in the **persist directory** (`LDMUX_PERSIST_DIR`,
  `%S/ldmux` under systemd), never in the state directory: that one is `$XDG_RUNTIME_DIR`, a
  tmpfs, so a credential kept there is re-minted on every boot. The bearer token, the mesh
  static key, the pinned phone key and the PSK all live in the persist directory, and a
  daemon finding them in the old place moves them across once. Phone secrets are AES-256-GCM
  under a non-exportable Android Keystore key, in private preferences.
- Held text in batch mode never leaves the process. It is not persisted and the
  screen shows a count rather than the text when the channel is public.
- **The decision and its alternatives** are recorded in
  [`docs/adr/0029-the-session-rides-raw-custom.md`](adr/0029-the-session-rides-raw-custom.md); this
  section is the summary and the audit.
- **Why a session layer sits inside MeshCore.** Its own envelope, for the payload types that
  carry text, is AES-128-**ECB** with no nonce or IV, a HMAC-SHA256 tag truncated to **two
  bytes**, and a static-static ECDH secret reused in both directions with no rekey (firmware
  evidence: `Utils.cpp`'s `aes.encryptBlock` loop and `resetHMAC(shared_secret, PUB_KEY_SIZE)`,
  `MeshCore.h`'s `CIPHER_MAC_SIZE 2` under a `// V1` comment, and issue #259's "right now we
  never rekey at all"). That is deterministic, forgeable at 1 in 65,536 per attempt, and has no
  forward secrecy. A terminal session needs none of those properties to hold, so ldmux uses the
  payload type the firmware reserves for it: `RAW_CUSTOM` (0x0F), documented in `Packet.h` as
  "custom packet as raw bytes, for applications with custom encryption, payloads, etc". Nothing
  is encrypted twice and no channel key is involved: the packet is addressed to the peer's radio
  key and its payload is the sealed record.
- **The session layer, checked against the Noise spec** (audited 2026-11-06): `snow` keeps a
  separate send and receive `CipherState`, so the half-duplex link cannot reuse a key and nonce
  across directions (Noise §12.2 calls the alternative catastrophic); the nonce is the 32-bit
  record counter, a retransmission is sealed with a fresh one, and `u32::MAX` is refused; rekey is
  a **fresh handshake**, never `Rekey()`, which would keep the nonce while changing the key; the
  filter tag verifies in constant time and is derived as `HMAC(psk, label)` rather than using the
  PSK directly; the clear header (kind, index, counter) needs no associated data because each
  field either selects the key or is the nonce, so any change fails the tag; and the prologue
  names the suite, so mismatched peers fail the handshake instead of deriving different keys from
  the same flags.
- **What the session does not give**: no post-compromise security (no ratchet), no traffic
  analysis resistance (record sizes follow the text, and the L2 path and node hashes are plaintext
  by MeshCore's design), and no protection against a leaked pairing code, which carries the PSK
  and the bearer token together.
- **A public channel changes nothing about confidentiality.** The channel is a
  frequency and a preset, not a trust decision: records travel as direct
  packets addressed to the peer's public key (`RAW_CUSTOM_DIRECT_HEADER = 0x3E`,
  `crates/companion/src/codec.rs`), each one sealed afresh under the session
  (`crates/session/src/session.rs`, handshake `Noise_IKpsk2_25519_ChaChaPoly_SHA256`),
  and the radio only ever receives sealed bytes. Anyone in range on the public
  preset can record the ciphertext and count the traffic, and cannot read it or
  forge it without the PSK. What a public channel does cost is shared airtime,
  which is why batch mode is forced there.

## 8. Failure modes

| What fails | What happens |
|---|---|
| a handshake gets no answer | it is asked again with fresh keys at 8, 16 and 32 seconds, then the connection is rebuilt |
| a record goes unanswered | silence is measured from the last record the link carried in either direction, so an idle shell cannot look dead. On the phone, 20 s with something outstanding ends the session (`SILENCE_FAILURE_MS`); on the host the ARQ's own ladder fails at about 62 s and the carriage-silence bound is 75 s |
| the carriage stops carrying anything | 75 seconds with a record outstanding ends the connection, so `supervise` can build another. The ARQ's own ladder usually fires first |
| the host restarts | the keeper keeps the shells, the phone reconnects and re-attaches, and the desk window is told when a shell moves |
| the daemon restarts | the phone's session ends and its retry ladder rebuilds it. A shell the phone created survives on the host |
| the phone takes a shell the desk has | the desk is told and stops, the pane is fitted to the phone, and when the phone lets go the pane is put back to the size it displaced |
| the app is backgrounded | pages detach after the platform grace, the shell stays in the keeper, and returning re-attaches |
| the radio's airtime allowance runs out | the host stops sending **new** records until the hour refills (`records_left` from the carrier's countdown), while acks and retransmissions still go, so the link stays alive and the pane stops updating; a record the firmware itself refuses is handed back to the retry ladder so it does not count as a lost attempt, and `last_error` says what happened |
| a mesh setting is malformed | the mesh link is disabled with a message and the daemon keeps running, because a bad key must not kill the shells |

## 9. Airtime

**One number, computed from the tuple the radio reports.** At the deployed tuple
(910.525 MHz, 500 kHz, SF7, CR5) a full 172-byte record is **69.2 ms** on air, and a
one-percent duty cycle buys about **520** of them an hour. The wire carries the budget in
one byte, so a host reports at most 255; `ldmux mesh status` therefore reads `255` on a
fresh link and counts down as records go out. Two other tuples for scale: 62.5 kHz at SF7
is 553 ms, and 250 kHz at SF11 is 1542 ms — the same code, so a different preset is a
different budget, and nothing here is a constant.

The figure lives in one function, `time_on_air_ms` (`crates/companion/src/codec.rs`), used
by the host radio, by the app before it retunes, and by every budget below.

- One screen update or one keystroke is one record. A record that does not fit waits
  for the next one.
- The app applies its own 36 second per hour meter by default, with a checkbox in
  settings. In 869.4 to 869.65 MHz the meter is a condition of use, not a
  preference: the app turns it on and says so.
- Batch mode exists because a public channel turns every keystroke into a broadcast.
- The bridge coalesces screen updates and lets input echo through ahead of them.
- **The host advertises what it is.** A flooded self-advert goes out when the link opens and every
  `LDMUX_MESH_ADVERT_SECS` (default half an hour), because a peer behind a repeater cannot learn a
  route to this radio any other way. An advert is one record of about 130 bytes: roughly 0.05 % of
  a 1 % allowance at the deployed tuple.
- **The host gates on the countdown it computed, not on the firmware's limiter.** MeshCore's
  `Dispatcher` charges measured airtime against a budget, but its factor is 1.0, which is a 50 %
  duty cycle (`getAirtimeBudgetFactor()`, `meshcore/src/Dispatcher.cpp`) — not the 1 % a European
  band allows. So the serial carrier computes the hour's allowance from the tuple the radio
  reports, publishes it as `recordsLeftThisHour`, and the session loop charges it per new record
  (`poll_budget`, `crates/daemon/src/mesh.rs`). The ARQ's limit is per poll, so a count that was
  not decremented would have allowed that many records every pass. Acks and retransmissions are
  not charged: they are what keeps the link alive. `ldmux mesh status` shows the countdown.

## 10. Platforms

| Target | State |
|---|---|
| Linux x86_64 with glibc, systemd user units | supported, the machine this is built and deployed on |
| Linux aarch64 | the flake evaluates, nothing has been built or run |
| Linux musl | not supported, the binaries link glibc |
| macOS daemon | not supported yet |
| Windows | not supported, not planned |
| Android client | done, verified on a device |
| iOS client | planned, and tailnet only: the radio route's MeshCore library is Java for Android and would have to be written again for iOS. Needs a macOS host for the xcframework |
| LoRa route, Android plus host | built and run on real radios, multi-hop recovery not implemented |

## 11. Bringing up the two radios

**One radio per side, each configured by the device that owns it.** The host radio
stays plugged into the host over USB, where the daemon drives it and can retune it.
The phone radio is powered anywhere and belongs to the phone over Bluetooth LE,
where the app reads and retunes it. A radio is driven by one owner at a time: the
firmware takes one companion session and the daemon takes an exclusive lock, so
plugging both radios into the host is harmless but gains nothing, and the phone
cannot use a radio the host is holding.

1. Flash both boards with MeshCore firmware that includes the companion
   environment, one `usb` build for the host and one `ble` build for the phone.
2. Name the host's device with `LDMUX_MESH_SERIAL`, for example
   `/dev/serial/by-id/usb-Espressif_...-if00`. The daemon opens it, reads the tuple
   the radio reports, and drives it. Nothing else runs between the daemon and the link,
   and the phone drives its own radio over Bluetooth LE.
3. The host radio's public key needs no configuration: the daemon reads it from
   `SELF_INFO` when it opens the radio, and a code made by a daemon that owns its
   radio carries it. `LDMUX_MESH_SELF` still overrides it, for a host that pins a
   key. A code with no key cannot be used, and re-scanning it cannot help.
4. Generate the session PSK once (`openssl rand -hex 32`), or set `LDMUX_MESH_PSK`: the
   daemon stores it in the persist directory (`mesh-psk`, 0600) and keeps it across
   restarts.
5. **The two radios must be on the same tuple** (frequency, bandwidth, spreading
   factor, coding rate). The host's is read from the radio it drives: `ldmux mesh
   status` prints it (`preset 910.525 MHz  500.0 kHz  SF7  CR5`) and `ldmux mesh
   preset <MHz> <kHz> <SF> <CR>` sets it, validated by the firmware. The phone's is
   set in the app under Settings, `phone's radio`, `edit`.
6. Pair the phone with `ldmux code --mesh`. The code carries the session keys, the
   host radio key, the tailnet address and token, **and the tuple the host's radio
   is on**. So the app can compare the two radios itself: when they differ, the radio
   panel shows a `desktop's radio` row with the host's tuple and one press to
   `match` it. Matching compares only the four numbers the firmware validates —
   power and path width are local choices. Getting this wrong without the code was
   silent: two radios on different tuples simply cannot hear each other, and the
   only symptom is a link that never comes up.

**Repeaters: what has to be true, and what is in place.**

A raw record is a **direct** packet, and the firmware delivers a `RAW_CUSTOM` packet only
when its route is direct (`Mesh.cpp`, `case PAYLOAD_TYPE_RAW_CUSTOM`: it checks
`pkt->isRouteDirect()` and the flood branch is commented out), so a flooded raw record would
be transmitted and then ignored by every receiver. Direct means the sender must know a path.

1. **The host makes itself known, and the peer must then ask for a route.** A companion does not
   advertise by itself, so the daemon floods a self-advert (`CMD_SEND_SELF_ADVERT` with mode 1)
   when the link opens and every `LDMUX_MESH_ADVERT_SECS` (default 1800; `0` turns adverts off).
   Repeaters forward it and its path accumulates, so the phone's radio learns a contact.
   **An advert does not teach a route on its own**: `BaseChatMesh::onAdvertRecv` never touches
   `out_path`. A route is learned from a path return, which MeshCore sends when a node receives a
   **flooded request**: `onPeerDataRecv` answers a flood `PAYLOAD_TYPE_REQ` with
   `createPathReturn(...)` sent flooded, and `onContactPathRecv` stores the reversed path as the
   contact's `out_path`. The companion command for that is `CMD_SEND_PATH_DISCOVERY_REQ` (0x34,
   which is a telemetry request forced to flood), and it needs the host in the radio's contact
   list first (`CMD_ADD_UPDATE_CONTACT`, 0x09). See the audit for the state of that step.
2. **The host replies along the path the request arrived by.** Each record that crosses
   repeaters arrives with its hops in travel order; `PeerPath::reversed` turns that into a
   usable out-path (per `hash_size` bytes, not per byte) and the radio thread sends every
   record along it. Until one arrives, records go zero-hop, which reaches only the radios in
   earshot.

Consequences worth knowing:

- **Timing follows airtime.** One record at 62.5 kHz SF7 is 553 ms, and each repeater holds a
  packet for a random delay of up to two airtimes, so the session's first retry is
  `max(2 s, 6 x airtime)` (`Config::with_airtime`), handed to the session by the carrier
  (`Bound::airtime_ms`) and by the app (`ldmux_session_set_airtime`). A fixed two-second retry
  would duplicate records the peer already has and spend the hour's allowance doing it.
- **Paths move.** The phone re-asks for its contact every 45 s while it has no route and every
  10 minutes once it has one, so a host that becomes reachable mid-session becomes reachable
  without a reconnect — provided the radio's contact carries an out-path, which is what the
  step above has to establish.
- **What is proven and what is not.** The reverse-path reply, the zero-hop fallback, the
  per-`hash_size` hop reversal and the airtime-scaled timers are unit-tested. Nothing in this
  tree has yet run through a real repeater: the two radios on the bench hear each other directly,
  so the path is always empty there. The first three-radio test is what proves the route-learning
  step end to end.
- **A path is not authenticated by MeshCore.** Route integrity is an 8-bit prefix comparison per
  hop (issue #1775 in the firmware tracker), so the session's Noise layer, not the path, is what
  authenticates the peer.

Without `LDMUX_MESH_SERIAL` this desktop has no radios; nothing else about the daemon
changes.

## 11a. Shell marks: how a shell says it is done

`ldmux buzz` marks a shell — `done` when an agent finished a turn, `attention` when it wants a look —
and the app draws it as a small square beside that shell's row in the shell list. Opening the shell
clears it, because opening it is reading it. It is **not** a notification and not a write: no claim,
no pty, no output, and it never carries the shell's contents (a kind, an id, an optional operator
line of at most 80 bytes).

A phone that is not connected cannot be pushed to, and the offline mode is exactly "no internet at
all", so delivery is two different answers to the same question:

- **Tailnet: a presence connection.** The launcher's socket says `hello` with the `buzz` feature and
  beats once a minute instead of every four seconds; the daemon gives such a connection a 150 s idle
  window (`PRESENCE_IDLE_MS`) so a page's cadence is not imposed on it, and keeps the foreground
  service up while the phone is paired. A connection that has just said hello is handed the whole
  mark table (`{"type":"marks"}`), which is how the marks made while it was away arrive; every later
  one is a `{"type":"buzz"}` push.
- **Radio: replay on attach and on list.** Keeping a radio session alive to be reachable would cost
  about twelve records an hour of a budget that is a few hundred, so the host keeps one mark per
  shell (`MARK_TTL_MS` 10 min, `MARKS_MAX` 64, coalesced per shell for 5 s) and sends the table with
  the phone's `Sessions` answer and with its `Attach` (`Message::Marks`, version 9). A version-8 phone
  is never sent the tag; a version-9 phone against an older host simply never sees one, which reads
  as "no marks".

The daemon holds the table (`crates/daemon/src/buzz.rs`), the claim clears a shell's mark (so both
routes obey one rule), and the desk hears it too: an attached window asks for `buzz` and writes one
BEL byte for its own shell, so whatever emulator the operator uses makes the noise they configured.

A notification is the optional second way of being told: a setting, off by default, tailnet only,
with a sounding and a silent channel (a channel's importance is immutable after creation, so honouring
a sound toggle on Android 8+ requires two). Tapping one opens that shell: if it is already a page the
app switches to it, and if it is not it becomes another page, which the header counts.

## 12. Build and test

- `scripts/test-parallel.py` runs every Rust test binary in a pool. It is the gate.
- `scripts/android.sh test` runs the Kotlin JVM suite, including the cross-language
  wire fixture generated by the Rust side.
- `scripts/android.sh build`, `install`, `release` and `bundle`. A release needs
  `packages/mobile/keystore.properties` or `LDMUX_KEYSTORE*`, reads its version code
  from `git rev-list --count HEAD`, refuses to package without both native libraries
  on every shipped ABI, and carries the licence texts collected into `dist/licenses`.
- `nix develop` provides the toolchain and `LDMUX_VT_LIB_DIR`. The terminal engine is
  libghostty-vt, built from one pinned Ghostty commit.
- **There is no CI.** Nothing runs on push: the gate is the four commands in the README's Development section, run by whoever is about to push. `nix flake check` is the one that also builds in the sandbox.
  It does not build the Android artifact, by decision.

## 13. Where the code is

`crates/protocol` is the one place a wire format is defined, for the daemon RPC, the
keeper, the pairing link and the mesh terminal protocol. `crates/session` is pure and
does no I/O. `crates/daemon` is the composition: registry, ownership, transports,
bridge. `packages/mobile` never defines a wire format of its own and never holds a
secret in plain text.

The open work is in `docs/RELEASE-AUDIT.md`, and the decisions with their reasoning
are in `docs/adr/`.
