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
pushed, because the radio cannot reach a phone that is not connected. Version 10 added the **shell** to each `Sessions` entry
(`bash`), behind the same kind of flag: the launcher draws the folder and the shell on a row's
second line, because a row's name is the conversation and the line under it has to say where the
shell is and what it is.

**The channel is the tuple and the path width; power is the third register.** `ldmux mesh preset
<MHz> <kHz> <SF> <CR> [--path N] [--power dBm]` tunes the host's own radio, `--path` sets the path
hash width in bytes, and `--power` the transmit power — which the firmware keeps apart from the
tuple and which a phone can set too, so it is a register two owners share rather than part of the
channel. `ldmux mesh status` prints all three on the preset line. The firmware keeps the tuple and
the width apart, but a network does not: MeshCore's public presets name neighbours in **one** byte, so a
repeater's hash is one byte, and a node that names them in two cannot read a path that repeater
built. ldmux's own channel is two bytes on both ends, because two bytes per hop is its default and
one byte collides at ≈20 co-channel repeaters; the public channel is one byte by the
community's convention. The phone takes the width from its preset and the host from this flag, and
the host publishes it in the pairing code (`hostPreset`'s fifth number), so the phone's one-press
`match` copies the whole channel and keeps only its own transmit power. A direct zero-hop packet
carries no path, which is why a width mismatch is invisible until a repeater is in the way.

**A newline in `Input` is a paste, and the host decides what it means.** The pane is the only place
that knows whether it negotiated bracketed paste (`CSI ? 2004 h`), so `Profile` carries that flag
(refreshed from every screen read, because programs toggle it at runtime) and the host encodes each
`Input` record for it: text with no newline passes through, pasted text is wrapped in `CSI 200 ~` …
`CSI 201 ~` with ESC stripped so a clipboard cannot end the paste early, or typed line by line with
carriage returns where the pane never asked for the markers. Typed Enter arrives as a carriage
return, which is why the newline is the marker and a carriage return is not: a phone that turned
every pasted newline into a carriage return ran each pasted line as a command.

A `sessions` entry carries who started the shell and who holds it now, each as one byte from a
fixed vocabulary, because two length-prefixed words made the worst case one byte wider than a record
has to carry.

`Commit.flags`: bit 0 alternate screen, bit 1 the shell has exited, bit 2 **idle** — the pane has
stopped changing and this commit carries no rows. The idle bit is additive (unknown bits are
ignored), it is sent once per burst ≈700 ms after the last row, and it exists so the phone
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
| a record goes unanswered | silence is measured from the last record the link carried in either direction, so an idle shell cannot look dead. On the phone, 20 s with something outstanding ends the session (`SILENCE_FAILURE_MS`); on the host the ARQ's own ladder fails at ≈62 s and the carriage-silence bound is 75 s |
| the carriage stops carrying anything | 75 seconds with a record outstanding ends the connection, so `supervise` can build another. The ARQ's own ladder usually fires first |
| the host restarts | the keeper keeps the shells, the phone reconnects and re-attaches, and the desk window is told when a shell moves |
| the daemon restarts | the phone's session ends and its retry ladder rebuilds it. A shell the phone created survives on the host |
| the phone takes a shell the desk has | the desk is told and stops, the pane is fitted to the phone, and when the phone lets go the pane is put back to the size it displaced |
| the app is backgrounded | pages detach after the platform grace, the shell stays in the keeper, and returning re-attaches |
| the radio's airtime allowance runs out | the host stops sending **new** records until the hour refills (`records_left` from the carrier's countdown), while acks and retransmissions still go, so the link stays alive and the pane stops updating; a record the firmware itself refuses is handed back to the retry ladder so it does not count as a lost attempt, and `last_error` says what happened |
| a record is larger than the path leaves room for | the limit is `min(171, 172 − path)`, advertised on bind and revised when the route changes; a chunk queued at the old size is refused by the builder, its attempt is given back, and the reconnect re-sizes |
| nothing is heard and nothing answers | the radio's own stats (`CMD_GET_STATS`) report the noise floor, and the RSSI and SNR of the last packet it heard: `mesh status` prints them, so "nothing heard yet" is a measurement rather than a missing feature, and a reading is what distinguishes "not being heard" from "heard and not answered" |
| a scoped mesh drops our floods | the session still works wherever a direct route exists, because scoping gates floods only; discovery needs the region, so `ldmux mesh status` prints the scope and the log names a scope learned from the air (ADR-0033) |
| a mesh setting is malformed | the mesh link is disabled with a message and the daemon keeps running, because a bad key must not kill the shells |

## 9. Airtime

**One number, computed from the tuple the radio reports.** At the deployed tuple
(910.525 MHz, 500 kHz, SF7, CR5) a full 172-byte record is **75.3 ms** on air; at the
MeshCore public preset's 62.5 kHz/SF7 it is **602.6 ms**, and at 250 kHz/SF11 it is
**3.66 s** — the same code, so a different preset is a different budget, and nothing here is a
constant. The firmware's own formula is the one used: 32 preamble symbols at SF8 and below
(`preambleLengthForSF`, `src/helpers/radiolib/RadioLibWrappers.h:56`), 20 explicit-header symbols,
CRC on, and low-data-rate optimisation once a symbol is 16 ms or longer.

The figure lives in one function, `time_on_air_ms` (`crates/companion/src/codec.rs`), used
by the host radio, by the app before it retunes, and by every budget below.

- **The allowance is band-aware, per ETSI window.** `869.4-869.65 MHz` allows 10 % and ldmux holds
  itself to **1 %** there (36 s an hour), and the app will not let that meter be turned off.
  `868.0-868.6 MHz` is **1 % by law**, and `868.7-869.2 MHz` is **0.1 %** (the alarm band: 3.6 s an
  hour, where a single 62.5 kHz record is 0.6 s). Everywhere else — including 902-928 MHz, where
  MeshCore's US/Canada preset sits — there is no duty cycle to comply with, so the allowance is
  **10 %** (360 s an hour): a fifth of what the firmware permits and less than half the Meshtastic
  cap. At 1 % a single typed command line at 62.5 kHz was the whole hour, which made the meter, not
  the radio, the thing that ended a session. The table used to meter `868.0-868.6` at 10 %, ten
  times its legal duty cycle, and no shipped preset reaches it — but `ldmux mesh preset` does.
- One screen update or one keystroke is one record. A record that does not fit waits
  for the next one.
- Batch mode exists because a public channel turns every keystroke into a broadcast.
- The bridge coalesces screen updates and lets input echo through ahead of them.
- **An advert is a maintenance broadcast, not the way a peer finds us** (ADR-0032), and it is
  **off by default** — the firmware's own companion default. `LDMUX_MESH_ADVERT_SECS=<seconds>`
  turns one on (3600 keeps the node in other operators' contact lists for about 0.07 % of a
  ten-percent allowance at 62.5 kHz). The community's
  guidance is far longer still — repeaters are told 47 hours — because an advert is airtime every
  node in range pays for. A route is requested on demand instead: a flooded `REQ` of ≈40 bytes and
  a direct reply of ≈40 bytes plus the path, once per attempt rather than once per interval.
- **Every broadcast is charged to the meter.** An advert and a route request are each counted as one
  record; an advert is smaller than a record at every tuple, so this over-counts rather than
  under-counts.
- **The host gates on the countdown it computed, not on the firmware's limiter.** MeshCore's
  `Dispatcher` charges measured airtime against a budget, but its factor is 1.0, which is a 50 %
  duty cycle (`getAirtimeBudgetFactor()`, `meshcore/src/Dispatcher.cpp`). So the serial carrier
  computes the hour's allowance from the tuple and the frequency, publishes it as
  `recordsLeftThisHour`, and the session loop charges it per new record (`poll_budget`,
  `crates/daemon/src/mesh.rs`). The wire carries the count in one byte, so the published figure caps
  at 255 even when the policy allows more (at 500 kHz/SF7 ten percent is ≈4770 records); the spend
  itself is counted exactly. The ARQ's limit is per poll, so a count that was not decremented would
  have allowed that many records every pass. Acks and retransmissions are not charged: they are what
  keeps the link alive. `ldmux mesh status` shows the countdown and the tuple it came from.

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


### The client's look and its voice

The Android client has **six themes**, and they are a contract rather than a decoration: one
`Palette` of thirteen tokens plus a terminal palette, one monospace and one proportional face per
theme, and — for all but the first — three session cues. `Dark` is the default and is silent on
purpose; `Forest`, `Air`, `Daybreak`, `Nerv` and `Gruvbox` each answer a session event in one
*material*.

- **The tokens are the API.** `Bg`, `Surface`, `Raised`, `Line`, `Edge`, `Fg`, `Dim`, `Muted`,
  `Accent`, `Chosen`, `Danger`, `Warn`, `OnAccent` are read by ~200 call sites; each theme supplies
  all of them, and a switch recomposes them because they are getters into snapshot state.
- **The tones are pinned, not eyeballed.** `ThemePaletteTest` recomputes every ratio from the
  literals: body ≥ 9:1, the second voice ≥ 6:1 and ≥ 1.3× the note, notes 4.5-6.6:1, every boundary
  and focus ring ≥ 3:1, and every surface a visible step off the one below it. It also pins the
  *ordering* (body > second voice > note > hairline), which is what stops a set of individually
  reasonable numbers from reading as a flat page.
- **Dark marks a choice with lightness, not with hue**, and that is asserted: its accent, chosen
  state, terminal cursor and selection wash are achromatic to the third decimal. The other four
  carry a hue in the same tokens.
- **The surfaces are tinted, the page is not.** Forest's ground is grey and its panels carry ≈0.02
  chroma of pine; Air's page is pure white with blue-grey rules; Daybreak's is white with a warm
  grey. A tint below ≈0.01 does not read as a cast at all, which is why the first Forest pass looked
  grey.
- **The cues are synthesised, not sampled.** `packages/mobile/tools/make-theme-sounds.py` builds
  all fifteen from modal-synthesis arithmetic (phase-zero partials, higher partials damping faster,
  a 1 ms head and a 5 ms tail, DC-blocked, each peak-normalised to its own target because equal peak
  is not equal loudness) and refuses to write a file outside its spec. Nothing is downloaded and no
  cue carries a licence.

## 11. Bringing up the two radios

**What the firmware actually guarantees, checked against `MeshCore` at
`companion-v1.17.0` on 2026-11-07** (the tag every command here exists in, so ldmux needs
stock firmware and not a fork):

- Frames over USB/UART are `0x3C <len lo> <len hi> <payload>` in and
  `0x3E <len lo> <len hi> <payload>` out, with a **16-bit little-endian payload-only
  length** and `MAX_FRAME_SIZE 176`. Over Bluetooth LE there is no delimiter at all: one
  GATT write is one frame, and one notification is one frame. A frame longer than 176 is
  **truncated** in and refused out, silently, with no error frame.
- `DEVICE_QUERY` (`0x16`) carries the app's target protocol version in byte 1, and the
  firmware **latches** it there, which is what selects the V3 message frames (we send 3).
  A latched value means the first `DEVICE_QUERY` of a connection decides the shapes.
- `DEVICE_INFO` (`0x0D`) is **82 bytes**: contacts ×2, channels, the BLE PIN, build date,
  manufacturer, version, then the client-repeat byte (v9+) and **`path_hash_mode` as the
  last byte** (v10+). The PIN bytes can read 0 while the radio is enforcing a random
  session PIN, so 0 is not "unprotected".
- `SET_RADIO_PARAMS` (`0x0B`) carries frequency in **kHz**, bandwidth in **Hz**, SF and CR,
  plus an optional repeat byte — and **no path width**, which is why the width has a
  command of its own. `SET_RADIO_TX_POWER` (`0x0C`) is **one signed byte**, window
  −9…max. `SET_PATH_HASH_MODE` (`0x3D`) is `[0x3D][0][mode]`; mode ≥ 3 answers
  `ILLEGAL_ARG`, while a *malformed* frame (byte 1 not 0, or shorter than three) answers
  `UNSUPPORTED_CMD` instead — two different codes for two different mistakes.
- `SEND_RAW_PACKET` (`0x41`) is a priority byte plus a whole MeshCore packet, and
  `RAW_CUSTOM` (payload type `0x0F`) is **processed only when it arrives by direct
  routing**; it is not flood-routed. The session therefore rides a direct header and can
  never be carried by a flood, which is why `RAW_CUSTOM_DIRECT_HEADER` is the only header
  the codec will build for it.
- **No fragmentation exists in v1.17.** `PACKET_FRAME_FRAGMENT` (`0x91`) appears in a
  community document but in no firmware at that tag, so 176 bytes is the hard ceiling for
  one frame and our record size is chosen under it.
- The path width is **per packet on the air** (the top two bits of `path_len`); the
  firmware's stored mode decides only what the node **originates**. Forwarding is
  width-agnostic from v1.14, so a one-byte phone and a two-byte host still hear each
  other; what breaks is reach *through* repeaters older than v1.14, which drop two- and
  three-byte packets without a word.
- **A radio has no notion of a client.** Frames carry no session id and no request tag,
  radio parameters and the width are global and persistent, and a write from one app is
  the device's state for the other with **no notification of the change**. That is why a
  retune from another app is invisible here until we ask again, and why a radio held by a
  second app fails in the way our "another app holds this radio" wording describes.

**A record's size is a property of the route, not of the link.** A raw send is
`[0x41][priority][header][path byte][path][record]` inside the firmware's 176-byte frame, so every
path byte costs a record byte, and a radio does not push a frame it cannot fit in that same 176
bytes — a direct record's arriving packet is 2 bytes plus the record. The limit advertised to the
session is therefore **`min(171, 172 − path)`**: 171 is the receive-log ceiling
(`RECORD_RECEIVED_MAX`) and the rest is the path. The session is sized at the handshake, so the
carrier reports the limit again when the path changes (`Event::Limit`); a chunk already queued at
the old size is refused by the packet builder, its attempt is given back, and the reconnect re-sizes
to what the route actually leaves. A **fixed** 172 was the old answer, and it was refused by
ldmux's own builder at the first path byte — so a full record only ever worked at zero hops.

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
   set in the app under Settings, `phone's radio`, `edit`. `ldmux mesh status` prints the host's
   path-hash width on the same line (`preset 910.525 MHz  500.0 kHz  SF7  CR5  path 2 B`), so the
   two radios are compared over the whole tuple rather than part of it.
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

0. **An advert cannot be the mechanism.** A repeater drops an advert once it has travelled
   `flood_max_advert` hops — **8 by default** (`examples/simple_repeater/MyMesh.cpp:907`, upstream
   PR #2702 in v1.16.0). Measured on this machine's local mesh 2026-11-08 from the host radio's own
   receive log: adverts arrive at 4, 5, 6 and **8** hops, while `REQ` floods arrive at **18-29** in
   the same window. Two radios further apart than eight hops therefore cannot exchange adverts at
   all, and the community's own guidance keeps them rare anyway (repeaters are told 47 hours,
   companions never). So an advert is a **maintenance broadcast, off unless asked for**:
   `LDMUX_MESH_ADVERT_SECS` turns one on (3600 keeps the node in other operators' contact lists),
   and the firmware's own companion default is to never advertise. Nothing depends on it — the
   route comes from the request below (ADR-0032) — and an advert heard from a radio that does
   advertise still teaches a route through the receive log (§ step 3).
1. **A route is asked for, over a flood.** A raw record cannot be flooded, so the request rides a
   payload type repeaters already forward: `REQ` (0), the one MeshCore's own flooded path request
   uses. The packet is
   `[header 0x01][path byte][dest hash 1][src hash 1][LDRQ][requester key 32][nonce 4]` —
   destination addressed by the first byte of the peer's public key (`PATH_HASH_SIZE` is 1), a
   four-byte marker, and the requester's radio key. The firmware never reads the tail: with no
   matching contact the `REQ` fails its MAC and is ignored, and a repeater forwards it by route and
   hop limit alone. `CMD_SEND_RAW_PACKET` (0x41) transmits it as built, so a stock radio works.
   **The four-byte nonce is load-bearing**: MeshCore dedupes on `SHA256(payload_type ‖ payload)` in
   a 160-entry ring with no timeout (`Packet::calculatePacketHash`, `SimpleMeshTables`), at every
   repeater and every receiver, so a byte-identical second request never leaves the first repeater.
   The reply echoes that nonce for the same reason, and the route hint carries one too.
2. **The peer answers with its own path, direct.** A request arrives with the hops it crossed, in
   travel order (flooded packets *append* one hash per repeater, `Mesh::routeRecvPacket`), so the
   receiver reverses the hop groups and replies with a `RAW_CUSTOM` record: `[LDRR][path byte][path]`,
   carrying **the path it used towards the requester**. One rule serves both directions — the reply
   names the sender's sending-order path and the reader reverses it — so after one round trip both
   ends hold a route. Nothing here needs a contact, a telemetry permission or a firmware change.
   The reply is consumed on both ends before the session can see it (`mesh_serial::route_reply`,
   `AndroidRadioLink.onRawData`): it arrives before the session exists and would otherwise fail a
   tag and be counted as noise.
3. **A route heard on the air is adopted on the event.** Both ends receive
   `PUSH_CODE_LOG_RX_DATA` (0x88) for every packet their radio hears, path included, so an advert
   from the peer updates the route the moment it arrives. The radio's own advert-path cache is
   still read (`CMD_GET_ADVERT_PATH` 0x2A) as a fallback — it needs the **whole 32-byte key**,
   because the firmware's guard is `len >= PUB_KEY_SIZE + 2` even though it compares seven bytes —
   and the contact's `out_path` is a last resort (`RadioRoute.preferHeard`). A route older than two
   minutes is treated as unknown for a *new* connection, because a phone that moved holds a
   well-formed route to where it used to be and nothing refuses it.
   The contact-based **path return** (`CMD_SEND_PATH_DISCOVERY_REQ`, 0x34) is not used: it needs
   the peer in the radio's contact table and the responder's BASE telemetry permission, which
   defaults to deny (`telemetry_mode_base`, `NodePrefs.h:24`).
3. **A direct record teaches no route.** A direct packet loses one hash per repeater
   (`Mesh::removeSelfFromPath`), so a record that crossed any number of them arrives with an **empty**
   path. That is why the request is a flood and the reply carries its path explicitly.
   **Who knows what, and when.** The phone knows the host's radio key from the pairing code, so it can
   address a request before anything has ever connected. The host learns the phone's radio key from a
   request, a reply or the 36-byte **route hint** (`LDRH` and the phone's public key, sent before the
   session handshake), and **remembers it** in `<state>/mesh-peer` — without that, a daemon restarted
   while the phone had moved could not address anything to it and could only wait for an advert.
   Either end that lacks a route floods a request: at most every 10 s, and the host answers at most one
   per 5 s, so neither can be turned into an amplifier by a peer that repeats itself.
4. **Every reply follows the reverse of the path that was learned.** `PeerPath::reversed` flips the
   hop groups (per `hash_size` bytes, not per byte) and the radio thread sends every record along it.
   With no route, records go zero-hop, which reaches only the radios in earshot.
   **Zero hops is a route, not the absence of one.** "The peer is adjacent" and "no route is known
   yet" are different answers on the wire — an empty path is exactly what a direct send to a radio
   in the same room means — and collapsing them made the periodic read stay on its 15 s retry
   cadence for ever on a bench. A zero-hop `PeerPath` is kept.
   A route request's path and a reply's path are already absolute: a request that arrives with no
   hops came from an adjacent peer, and that is the route.

5. **A flood scope is followed, never invented** (ADR-0033). A MeshCore region gates *floods only* —
   `allowPacketForward` checks it when `isRouteFlood()`, so a direct record is forwarded on its path
   alone — which means a wrong scope costs discovery and never the session. Three facts decide the
   rule: an unscoped flood goes to the repeaters' wildcard `*` entry, whose flood flag is clear by
   default and which most meshes therefore forward; a scoped flood is forwarded only by repeaters
   carrying that exact tag; and the drop is **silent**. ldmux therefore reads the radio's own default
   scope (`CMD_GET_DEFAULT_FLOOD_SCOPE` 0x40, which returns the name **and** the 16-byte key), uses
   `LDMUX_MESH_SCOPE` only as an operator's pin (`<tag>` or `off`), and otherwise adopts a tag only
   when an **overheard scoped flood proves** one of the published region names — a scoped packet
   carries `HMAC-SHA-256(key, payload_type ‖ payload)`, so recomputing candidates is the only way to
   name a region, and a match is proof that the repeaters in the path carry it. A radio with no scope
   and no proof stays unscoped. Nothing is a setting, and `ldmux mesh status` prints the one line
   that says which of those happened.

Consequences worth knowing:

- **Timing follows airtime.** One 172-byte record at 62.5 kHz SF7 is 603 ms (the firmware's own
  formula: 32 preamble symbols below SF9, `preambleLengthForSF`), and each repeater holds a packet
  for a random delay of up to two airtimes, so the session's first retry is
  `max(2 s, 6 x airtime)` (`Config::with_airtime`), handed to the session by the carrier
  (`Bound::airtime_ms`) and by the app (`ldmux_session_set_airtime`). A fixed two-second retry
  would duplicate records the peer already has and spend the hour's allowance doing it.
- **Paths move.** The phone floods a route request whenever the route it holds is older than two
  minutes — on every connect attempt, when a shell is opened, and on a 10 s timer while it has
  nothing usable (`ROUTE_FRESH_MS`, `ROUTE_REQUEST_FLOOR_MS`) — and reads its radio's cache as a
  fallback every 60 s once it has one. The host asks the phone the same way while it has no route,
  and re-reads the cache whenever a record arrives (floored at 2 s). A route that turns out to be
  stale is dropped by the app, which retries zero-hop and asks again; on the host side **a zero-hop
  answer clears it**, because the phone back in earshot has no path and keeping the last repeater
  chain would send every reply into a route that no longer reaches it.
- **What is proven and what is not.** The route request and reply, the reverse-path rule, the
  zero-hop fallback, the per-`hash_size` hop reversal (both directions: `PeerPath::reversed` and
  `RadioRoute.reversed`), the advert-path decoding and the airtime-scaled timers are unit-tested.
  **Nothing in this tree has yet run through a real repeater**: the two radios on the bench hear each
  other directly, so the path is always empty there. The first on-air test — two radios far enough
  apart that a public repeater is the only way between them — is what proves the route-learning step
  end to end, and the diagnostics for it are the daemon log's `route` lines (`journalctl --user -u
  ldmux | grep route`) and the app's `mesh … route: N hop(s)` log entries.
- **Two bytes per path hop, and that is ldmux's default.** The firmware ships at mode 0, one byte:
  254 usable hashes, and 0x00 and 0xFF are reserved. At ≈20 co-channel repeaters a one-byte
  prefix collides with coin-flip probability, and a collision is not cosmetic — two repeaters with
  the same prefix both forward a direct message meant for one of them. Two bytes gives 65 536 and
  holds to a few hundred; the price is the hop ceiling — `hash_count × hash_size ≤ 64` (`Packet::
  isValidPathLen`), so **63 hops at one byte, 32 at two, 21 at three**. This mesh's own floods were
  measured at 18-29 hops on 2026-11-08, so two bytes leaves three to eleven hops of headroom and
  three bytes would be below the distance actually in use: the mode 2 upgrade is left alone when an
  operator chose it, but it is not a width ldmux would pick, and a path that hits the ceiling is
  dropped by `routeRecvPacket` without a word. Both ends therefore put a radio reporting mode 0 onto mode 1 at
  bind (`DEFAULT_PATH_HASH_MODE`, `set_path_hash_mode`), once, and never touch a radio an operator
  set to three bytes. The width is a preference about the node's *own* adverts only: a repeater
  forwards whatever width arrives (`MyMesh.cpp:491`), so this changes what peers hear from us, not
  what we relay.
- **ldmux keeps no contacts** (ADR-0030). A route comes from the advert-path cache, keyed by the
  peer's public key (`onDiscoveredContact` → `advert_paths[]`), which needs no contact entry on
  either radio: no auto-add policy, no contact cap to hit, and no per-contact record to grow stale
  or leak. The only contact-dependent route — `CMD_SEND_PATH_DISCOVERY_REQ` — needs the peer in the
  sender's contact table *and* the responder's BASE telemetry permission, which defaults to deny
  (`NodePrefs.h:24`), so a stock radio answers nothing. What the operator's MeshCore app keeps for
  messaging is that app's business, not the terminal's.
- **The advert-path cache is small, in RAM, and evicts by age.** Sixteen slots
  (`ADVERT_PATH_TABLE_SIZE`, `MyMesh.h:257`), zeroed at boot, oldest-lost first
  (`MyMesh.cpp:368-386`). On a crowded channel `ERR_CODE_NOT_FOUND` for a node heard a minute ago
  is normal, not a fault, so a single failed read never means "the route is wrong": both ends keep
  the route they have and re-ask on their cadence.
- **An advert goes out in the radio's default flood scope, when it has one.** A region-scoped radio
  sends the advert inside that scope (`default_scope_key`, `MyMesh.cpp:489-498`), so two radios on
  the same tuple but different scopes hear each other only for zero-hop traffic. Nothing to change
  in ldmux; it is one more thing "the same tuple" means.
- **A scope is a routing label, not a secret.** The name and the 16-byte key are public for every
  hashtag-style region, nothing about them goes on the air, and a scoped packet shows only a 16-bit
  HMAC over its own payload. Scoping therefore hides nothing and authenticates nothing; it decides
  only which repeaters carry a flood.
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
  ≈12 records an hour of a budget that is a few hundred, so the host keeps one mark per
  shell (`MARK_TTL_MS` 10 min, `MARKS_MAX` 64, coalesced per shell for 5 s) and sends the table with
  the phone's `Sessions` answer and with its `Attach` (`Message::Marks`, version 9). A version-8 phone
  is never sent the tag; a version-9 phone against an older host simply never sees one, which reads
  as "no marks".

The daemon holds the table (`crates/daemon/src/buzz.rs`), the claim clears a shell's mark (so both
routes obey one rule), and the desk hears it too: an attached window asks for `buzz` and writes one
BEL byte for its own shell, so whatever emulator the operator uses makes the noise they configured.

A notification is the optional second way of being told: one setting, off by default, tailnet only,
one channel, one notification per mark, which goes away when it is read or opened. It is never
posted while the app is on screen, because the square beside the row already says it. Tapping it
opens that shell: if it is already a page the app switches to it, and if it is not it becomes
another page, which the header counts.

**Exactly one notification in this app is persistent**, and it is not this one: a foreground service
may not run without a notification, so the one that keeps a background link alive exists while the
app is off screen, on the quietest channel there is, saying what is being kept (`2 shells open`).
It stops the moment the app comes back on screen.

## 11b. Shell titles: what a shell is working on

A shell is named by its kind, else by its directory's basename, so four agents in four folders read
as four folders. `ldmux title "Radio paths"` names it after the conversation instead, and every
surface that draws a row — the phone's list, `ldmux list`, the desk's picker — says that name.

- **One to three words, 24 bytes.** A title is a label, not a description: `Radio paths`, not
  `Investigating why the radio path goes stale`. Anything longer is refused with a sentence and a
  non-zero exit, because a row is a thing a thumb aims at.
- **The daemon owns the name.** It is the `SessionRecord.title`, persisted with the registry, so a
  rename survives a restart and a client that connects later asks nothing.
- **The agent's own session name follows, best effort.** A phone row and a `pi --resume` picker
  disagreeing about the same conversation is one name too many, so the same words are written to
  the agent's session file when one can be identified: pi appends the `session_info` record its own
  `appendSessionInfo` writes (`$PI_SESSION_FILE` names the file, and the last such record wins on
  load), and Claude Code gets the `custom-title` record its `/rename` writes, in the transcript that
  `~/.claude.json`'s `projects.<cwd>.lastSessionId` points at — and only when that transcript was
  touched in the last half hour, so an old conversation in the same folder is not renamed. Both
  files are append-only and both writes are one `O_APPEND` write of one line, refused when the file
  does not end at a line boundary. Neither can fail the command: the shell's title is the product.
- **The shell names itself.** Every child of an ldmux shell carries `LDMUX_SESSION_ID`, so the
  command needs no argument and cannot rename another shell by copying an id; `--session` is for a
  process outside a shell.

The skill in `contrib/skills/ldmux-shell` is what asks an agent to do it: name the shell once the
subject is clear, rename it when the subject changes, and `ldmux buzz` when the turn ends.

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
