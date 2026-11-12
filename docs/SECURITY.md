# Security

What each route protects, what it does not, and where the secrets live. The design is
`docs/ARCHITECTURE.md` §7; the radio decision and its alternatives are
`docs/adr/0029-the-session-rides-raw-custom.md`. What the radio redesign changes about this page is
in `docs/RADIO-PLAN.md` §5: the record kinds stay inside the same Noise session, `KIND_NEED_HANDSHAKE`
carries a PSK-authenticated tag so a stranger cannot force a handshake, and the phone's identity
becomes its own session static key rather than an install UUID.

## The tailnet route

- A bearer token in an `Authorization` header, checked at the WebSocket upgrade. The query string
  is not a credential: a URL reaches proxy logs, crash reports and shell history in ways a header
  does not, and no client sends one.
- The daemon listens only on the tailnet address or on loopback. There is no public listener and
  no way to disable authentication.
- A refused token answers 401 and a malformed upgrade for an accepted token answers 400, so the
  app can tell "wrong password" from "machine off".
- The attach socket used by desk clients is a separate Unix socket, mode 0600, with the peer uid
  checked.

## The radios

- **A record is a Noise message inside a MeshCore packet**, addressed to the peer radio's public
  key: `Noise_IKpsk2_25519_ChaChaPoly_SHA256` over `snow`
  (`crates/session/src/handshake.rs`). The vehicle is a `RAW_CUSTOM` direct packet when a route is
  known and a `REQ` flood carrying the sealed record when it is not (ADR-0037), and the wrapper is
  never trusted for confidentiality. MeshCore's own encryption is never applied, so no channel key
  is involved and the shared preset is a frequency, not a secret.
- **Why not MeshCore's channel encryption**: its channel envelope is AES-128-ECB with no nonce or
  IV, a HMAC-SHA256 tag truncated to two bytes, and one shared key per channel, which is
  deterministic and forgeable at 1 in 65,536 per attempt; per-contact traffic uses a static-static
  ECDH secret that is never rekeyed. The firmware labels group payloads "unverified" for the same
  reason (`Utils.cpp`, `MeshCore.h`'s `CIPHER_MAC_SIZE 2` under a `// V1` comment, issue #259).
- **What the session gives**: a nonce that never repeats (the record counter, a fresh one per
  retransmission, `u32::MAX` refused), a full 16-byte tag, separate keys per direction (so the
  half-duplex link cannot reuse a key and nonce), a replay window, forward secrecy per session from
  ephemeral keys, and rekey by a fresh handshake every 65,536 records or 12 hours rather than
  `Rekey()`, which would keep the nonce.
- **What it does not give**: no ratchet, so no post-compromise security; record sizes follow the
  text; and MeshCore carries the path, node hashes and traffic volume in plaintext by design, so an
  observer learns that two radios are talking, and how much, but not what.
- **Accepted trade-offs**: an unauthenticated flood costs one HMAC before any Diffie-Hellman
  (the 16-byte filter tag, a WireGuard-like addition on top of Noise); and records are not padded,
  because padding spends the same airtime as text and the medium already reveals volume.
- **Both radios ask each other for a route over a flood** (ADR-0032). A request is a MeshCore `REQ`
  carrying four marker bytes and the requester's **radio** public key — not the session key, and
  nothing secret: every advert publishes that key already. It is a broadcast, so every node in range
  repeats and sees that this radio exists and that it is asking a particular peer for a route. A
  route request is unauthenticated, like the advert it replaced: a forged one can only change where
  a reply is *sent*, and everything inside the session is sealed by Noise above it. Both ends rate
  limit (one reply per 5 s, one request per 10 s) so a peer cannot use either as an amplifier.
- **The phone's radio also advertises while it is trying to reach the host**, once when the link
  binds and then at most every three minutes while nothing has been heard, which publishes its node
  name and public key to the mesh. `LDMUX_MESH_ADVERT_SECS=0` on the host turns the host's own
  adverts off; discovery then rests on the route request alone.

- **A flood scope is a routing label, not a credential.** Its name and its 16-byte key are
  `SHA256("#" + name)` for every public region, and neither is ever transmitted: a scoped packet
  carries only a 16-bit HMAC over its own payload. Scoping hides nothing and authenticates nothing;
  it decides which repeaters forward a flood (ADR-0033). ldmux reads the scope from the radio rather
  than inventing one, because a repeater drops a flood scoped to a region it does not carry, silently.

## Secrets

- **The pairing code is the secret.** A `--mesh` code carries the session PSK, the host radio's
  public key and the bearer token together, printed to a terminal, so scrollback and shell history
  are inside the trust boundary. Re-pairing rotates all of it.
- **On the host**: the bearer token, the mesh static key, the pinned phone key and the PSK are
  0600 files in the persist directory (`LDMUX_PERSIST_DIR`, `%S/ldmux` under systemd), never in
  the state directory: that one is `$XDG_RUNTIME_DIR`, a tmpfs, so a credential kept there is
  re-minted on every boot and locks every paired phone out (ADR-0027).
- **On the phone**: secrets are AES-256-GCM under a non-exportable Android Keystore key, in
  private preferences, excluded from backup and from device transfer.
- **The host pins the phone's static key** on the first accepted handshake, and the handshake
  timestamp is bounded to a 5 minute window against a stored floor.

## Marks (`ldmux buzz`)

- **Local only.** The verb speaks the control socket (peer uid), so only a process on the host can
  make one; there is no wire path that lets a phone mark a shell.
- **No shell content.** A mark is a kind (`done` or `attention`), a session id, and at most 80 bytes
  the operator wrote, with control bytes stripped and whitespace collapsed before it is stored: a
  notification title is a place an escape sequence can lie.
- **Only a running shell**, and a repeat inside five seconds for the same shell is refused, so a
  loop cannot turn the phone into a bell. The table is bounded (64 shells, 10 minutes) and lives in
  memory only, so a restart forgets every mark.

## Grants and defaults

- **Clipboard writes are refused by default.** A program's copy is captured by the app, and the
  host refuses to deliver it unless the operator granted it: `ldmux clipboard-write on`. The app
  shows what a program tried to copy either way.
- **Held batch text never leaves the process** and is not persisted. On a public channel the
  screen shows a count rather than the text.
- **Nothing is collected**: no analytics, no crash SDK, no telemetry, no update check. The app
  talks to the paired host and to nothing else, apart from the on-device speech recogniser unless
  a cloud fallback is used, which the screen states at the time.
