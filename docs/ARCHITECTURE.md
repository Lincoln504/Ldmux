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
| LoRa batch | the same radios | typed text is collected on the phone and leaves the radio as one batch when the user sends it, which is forced on a public channel |

Batch is the mode name in the app, the docs and the settings row (`input: live |
batch`). The identifier in the code is still `MeshInputMode.ManualSync`.

Only one phone talks to one desktop at a time, and a public channel is classified
from the tuple the radio reports, never from a configured name.

## 3. Layers

- L1, the radios. A MeshCore companion radio on each end. The presets must match:
  frequency, bandwidth, spreading factor and coding rate.
- L2, the carriage. A frame with an `MG` header, a type, flags and a length, carrying
  one record. `crates/mesh` is the client side for the gateway, and
  `crates/companion` plus `crates/daemon/src/mesh_serial.rs` is the direct side.
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
- bearer token checked at the upgrade, case-insensitive scheme, query token also
  accepted for compatibility
- a plain GET answers 401 for a refused token and 400 for an accepted token whose
  upgrade was malformed, which is how the phone tells "wrong password" from
  "machine off"
- JSON control messages and binary output frames, `permessage-deflate` when offered
- one attach per connection, ownership checked before any write

Mesh terminal protocol, version 8:

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

Version 7 added shell control. Version 8 added the holder and the takeover. A
`sessions` entry carries who started the shell and who holds it now, each as one
byte from a fixed vocabulary, because two length-prefixed words made the worst case
one byte wider than a record has to carry.

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
- Host secrets are files in the state directory at 0600. Phone secrets are
  AES-256-GCM under a non-exportable Android Keystore key, in private preferences.
- Held text in batch mode never leaves the process. It is not persisted and the
  screen shows a count rather than the text when the channel is public.

## 8. Failure modes

| What fails | What happens |
|---|---|
| a handshake gets no answer | it is asked again with fresh keys at 8, 16 and 32 seconds, then the connection is rebuilt |
| a record goes unanswered | silence is measured from the last record the link carried in either direction, so an idle shell cannot look dead. 20 seconds with something outstanding ends the session |
| the carriage stops carrying anything | 75 seconds with a record outstanding ends the connection, so `supervise` can build another. The ARQ's own ladder usually fires first |
| the host restarts | the keeper keeps the shells, the phone reconnects and re-attaches, and the desk window is told when a shell moves |
| the daemon restarts | the phone's session ends and its retry ladder rebuilds it. A shell the phone created survives on the host |
| the phone takes a shell the desk has | the desk is told and stops, the pane is fitted to the phone, and when the phone lets go the pane is put back to the size it displaced |
| the app is backgrounded | pages detach after the platform grace, the shell stays in the keeper, and returning re-attaches |
| a budget is spent | the host stops sending data but always sends acks, handshakes and heartbeats |
| a mesh setting is malformed | the mesh link is disabled with a message and the daemon keeps running, because a bad key must not kill the shells |

## 9. Airtime

This link is 138 ms per record: 914.25 MHz, 500 kHz, SF8, CR5, which at a one
percent duty cycle buys about 261 records an hour. Everything below is read against
that number.

- One screen update or one keystroke is one record. A record that does not fit waits
  for the next one.
- The app applies its own 36 second per hour meter by default, with a checkbox in
  settings. In 869.4 to 869.65 MHz the meter is a condition of use, not a
  preference: the app turns it on and says so.
- Batch mode exists because a public channel turns every keystroke into a broadcast.
- The bridge coalesces screen updates and lets input echo through ahead of them.
- The host does not gate the radio's airtime when it owns the radio. The firmware
  runs the transmitter's duty cycle, and the host applies a second meter only when a
  gateway reports a budget.

## 10. Platforms

| Target | State |
|---|---|
| Linux x86_64 with glibc, systemd user units | supported, the machine this is built and deployed on |
| Linux aarch64 | the flake evaluates, nothing has been built or run |
| Linux musl | not supported, the binaries link glibc |
| macOS daemon | not supported yet |
| Windows | not supported, not planned |
| Android client | done, verified on a device |
| iOS client | not started, needs a macOS host for the xcframework |
| LoRa route, Android plus host | built and run on real radios, multi-hop recovery not implemented |

## 11. Bringing up the host radio

The phone's radio is the app's business. The host's is the operator's, and there is
no installer or provisioning screen for it.

1. Flash both boards with MeshCore firmware that includes the companion
   environment, one `usb` build for the host and one `ble` build for the phone.
2. Name the host's device with `LDMUX_MESH_SERIAL`, for example
   `/dev/serial/by-id/usb-Espressif_...-if00`. The daemon opens it, reads the preset
   the radio reports, and drives it. No gateway process runs.
3. Put the host radio's public key in `LDMUX_MESH_SELF`. The daemon drives the radio
   but does not read its key, and a pairing code without it cannot be used.
4. Generate the session PSK once into the state directory, or set
   `LDMUX_MESH_PSK`. `ldmux mesh status` reports the link, the radio, the budget and
   the last error.
5. Set both radios to the same operating point. The host prints its own at start,
   `ldmux mesh status` reports it, and the phone's radio panel can read a radio,
   adopt a public preset, or take a tuple by hand.
6. Pair the phone with `ldmux code --mesh`.

If `LDMUX_MESH_SERIAL` is unset, the older path applies: the daemon dials a loopback
gateway named by `LDMUX_MESH_ADDR` and learns nothing about presets, paths or
contacts.

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
- CI runs on every branch: `nix flake check`, clippy, the Rust pool, `cargo deny`.
  It does not build the Android artifact, by decision.

## 13. Where the code is

`crates/protocol` is the one place a wire format is defined, for the daemon RPC, the
keeper, the pairing link and the mesh terminal protocol. `crates/session` is pure and
does no I/O. `crates/daemon` is the composition: registry, ownership, transports,
bridge. `packages/mobile` never defines a wire format of its own and never holds a
secret in plain text.

The open work is in `docs/RELEASE-AUDIT.md`, and the decisions with their reasoning
are in `docs/adr/`.
