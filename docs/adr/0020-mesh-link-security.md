# ADR-0020 — Mesh link security: an end-to-end Noise session inside MeshCore

**Status:** Proposed (2026-10-06); partly implemented 2026-10-28 on the daemon side:
`crates/session` has the handshake, the record layout and the ARQ, and ADR-0023 decided the
crypto dependency. The phone calls that Rust session through a C ABI (ADR-0023, update 2026-10-28), tested in a JVM and not yet on a device. Padding, the pairing QR, the ML-KEM stage and the terminal encoding are not built.
The link beneath it: `crates/mesh`
and the daemon's `mesh.status`, verified against a real radio on 2026-10-28.

- **Date:** 2026-10-06
- **Related:** ADR-0017 (QR pairing), ADR-0016 (direct tailnet bind), ADR-0010
  (exclusive ownership), ADR-0019 (wire compression; referenced only), `docs/MESH-TRANSPORT.md`
- **Scope:** MeshCore is the target. Meshtastic is out of scope here.

## Context

`docs/MESH-TRANSPORT.md` investigates a terminal over MeshCore. Its earlier text said the
mesh profile shares "the entire security model" with the WebSocket path. That is false
over radio. The WebSocket path is plain `ws://` inside the tailnet (ADR-0016): WireGuard
supplies confidentiality, integrity and peer authentication, and the bearer token only
selects which tailnet peer may attach. A radio link has no tailnet. Every repeater, and
anyone within range, sees every packet.

**Requirement (user):** above-consumer-grade, end-to-end encryption on the MeshCore path,
so that the mesh and its repeaters see only opaque ciphertext.

### What MeshCore's own cryptography is

Read from the primary source (Jack Kingsman, "A Hitchhiker's Guide to MeshCore
Cryptography", 2026-01-20, re-fetched for this ADR) and the MeshCore packet-format and
payload pages. The author is not a cryptographer by his own statement, and his code
references are pinned to one firmware commit; treat this as a description of that
commit, which may have moved.

| Property | MeshCore (as documented) |
|---|---|
| Cipher | AES-128-ECB. Not CBC. |
| Padding | Zero padding to 16 bytes. The reference decoder strips trailing zero bytes, so a binary payload ending in `0x00` is corrupted. |
| Authentication | 2-byte MAC: HMAC-SHA256 truncated to 2 bytes (in the author's example code). |
| Key selection | 1-byte hash (first byte of the contact public key, or of SHA-256 of the channel key). With the 2-byte MAC, a wrong key passes both checks with probability about 2^-24 and decrypts to garbage. |
| IV | None. The 4-byte message timestamp plus the attempt number serve as an "ersatz IV" (the author's phrase). |
| Direct messages | Ed25519 identities; X25519 static-static ECDH. No forward secrecy. |
| Channels | One shared channel key. A hashtag room's key is the first 16 bytes of SHA-256 of the room name, so its keyspace is guessable: public by design. |
| Author's verdict | "Reasonably proportional to the threat model." He would not carry secrets over it. |

Consequences for us: it provides no meaningful confidentiality against a motivated
observer, a 16-bit forgery bound per packet, no forward secrecy, and it cannot be turned
off for the message types that use it. It is **an untrusted transport**.

One fact changes the layering. The MeshCore packet-format page lists type `0x0F`
(`PAYLOAD_TYPE_RAW_CUSTOM`) as "raw bytes, custom encryption", and the payload page says
"Custom packets have no defined format." MeshCore therefore applies **no** cipher to that
type. Whether the companion API can emit it is **unmeasured** (see Open items).

### Errors in an automated research summary, not repeated here

CBC (wrong: ECB); Ed25519 as quantum-resistant (wrong: Shor's algorithm breaks it);
XChaCha20-Poly1305 as nonce-misuse resistant (wrong: its 192-bit nonce makes random
nonces safe, but only SIV constructions such as AES-GCM-SIV resist misuse); WireGuard
with built-in post-quantum support (wrong: Rosenpass supplies a post-quantum-derived
pre-shared key to WireGuard; its site says it uses Classic McEliece and Kyber).

## Decision

### 1. Layering

An end-to-end secure session between the phone and an **ldmux gateway** (a separate
process next to the daemon, see `MESH-TRANSPORT.md` "Review"), carried as the opaque
payload of a MeshCore packet. MeshCore sees bytes; the daemon sees only what the gateway
decrypts.

Two carriages, in order of preference:

| Carriage | MeshCore adds | Status |
|---|---|---|
| **A. Raw/custom packet (`0x0F`)** | Nothing: no cipher, no MAC, no padding. | Preferred. Whether the radio/companion path can send and the repeaters forward it is unmeasured. |
| **B. Text message (DM type)** | Destination and source hash bytes (2), AES-128-ECB over `timestamp(4) + flags(1) + body`, padded to 16, MAC (2). | Fallback. Companion text cap is 160 B. Body must be UTF-8-safe unless the companion accepts binary (unmeasured). |

Under B our layer is **inside** MeshCore's: our ciphertext is encrypted again by
AES-128-ECB. That adds no strength, and leaks nothing, because ECB leaks only repeated
plaintext blocks and our ciphertext is uniformly random. MeshCore's 2-byte MAC is not
relied on; a packet that passes it is still rejected by our 16-byte tag.

Nothing in our design depends on any MeshCore key, identity, hash byte or MAC.

### 2. Handshake: Noise IKpsk2, PSK mandatory, ML-KEM optional

**Suite:** `Noise_IKpsk2_25519_ChaChaPoly_SHA256` (Noise revision 34).

- **IK**: the initiator (phone) knows the responder's (gateway's) static key before
  the first message, delivered by the QR (ADR-0017). No trust on first use on the phone.
- **psk2**: a 256-bit pre-shared key mixed in at the end of message 2 (verified:
  `psk2` places the `psk` token at the end of the second message; the spec requires
  PSKs to be secret with 256 bits of entropy and warns against using one PSK with
  more than one hash or outside Noise).
- **X25519, ChaCha20-Poly1305, SHA-256.** ChaCha20-Poly1305 because radios and phones
  need not have AES hardware; SHA-256 rather than BLAKE2s because `snow`'s ring
  resolver offers SHA-256/512 but not BLAKE2 (verified from the `snow` README), and ring
  covers X25519, ChaCha20-Poly1305 and AES-GCM. The lead states ring has no ML-KEM; not
  re-checked here.
- **No 0-RTT / early data.** In IK the first message is replayable and its payload goes
  to a not-yet-authenticated-by-reply peer (Noise's own payload-security table rates
  it "encrypted to a known recipient, vulnerable to replay"). Application bytes flow
  only after message 2.

**Handshake messages** (field layouts; derived from the IK token definitions):

| Message | Fields | Size |
|---|---|---|
| 1 (initiator) | type/version 1, filter tag 16, `e` 32, encrypted `s` 32+16, encrypted payload (timestamp 8) 8+16 | 121 B |
| 2 (responder) | type/version 1, `e` 32, encrypted payload (assigned session index 1) 1+16 | 50 B |

Each fits one packet. The **filter tag** is a 16-byte MAC over the message keyed from
the PSK, checked *before* any Diffie-Hellman, so an unauthenticated flood costs a MAC
and not a scalar multiplication. This is an addition of ours (the same idea as
WireGuard's mac1); it needs a reviewer, not a new primitive. The timestamp (wall
clock, 8 B) must be strictly greater than the last one accepted for that initiator
and within a window (default 5 minutes; a judgement call, long enough for a slow mesh
and clock skew); the gateway persists only that one number per pairing. This bounds
replay of message 1 to the window even if the gateway lost its state.

**What the PSK buys against a quantum adversary.**

- Session keys depend on the PSK as well as on X25519. A quantum computer that breaks
  X25519 (Shor) still lacks the PSK, so past and future *session confidentiality* stays
  safe while the PSK is secret.
- Authentication is not signature-based: forging an initiator or responder needs the
  PSK, not a discrete logarithm. Shor alone does not forge it.
- Residual leak: in `psk2`, message 1 is protected only by `es`/`ss`, so a recording
  adversary with a quantum computer recovers the phone's static *public* key and the
  timestamp from message 1. It does not recover session keys. `psk0` would close this,
  but then the responder must pick the PSK before decrypting the initiator's static key,
  which needs a clear pairing selector; the Noise spec itself notes that responder
  difficulty. Judgement call: `psk2`, because one gateway serves several phones.

**Limits of the PSK.**

- Symmetric, per pairing, provisioned out of band in the QR, 256 bits, rotatable by
  re-pairing. Its compromise removes this protection: an adversary holding the PSK
  *and* a quantum computer can decrypt recorded sessions.
- It adds no forward secrecy beyond what the ephemeral X25519 keys give classically.

**Optional stage: hybrid X25519 + ML-KEM-768.** Run *after* message 2, inside the
established transport, as a second exchange whose shared secret is mixed into new
transport keys. It covers exactly the residual case above (PSK later leaked *and* a
quantum adversary with recordings). Cost, once per session (sizes verified against
FIPS 203 secondary summaries: public key 1,184 B, ciphertext 1,088 B, shared secret 32 B;
FIPS 203 itself is final, published 2024-08-13):

| Item | Value |
|---|---|
| Fragments at 160 B each | 8 (public key) + 7 (ciphertext) = 15 packets |
| Airtime at about 189 B on air, per packet | about 0.3 s (SF7/125 kHz), 0.9 s (SF10/250 kHz), 7.5 s (SF11/62.5 kHz) |
| Total, whole stage | about 5 s, 13 s, 113 s respectively |

Airtime figures are computed here with the Semtech time-on-air formula from memory
(coding rate 4/5, 8-symbol preamble, explicit header, CRC, 3-hop path), **not measured
and not checked against the Semtech document**; they exclude retransmissions and
collisions. Fragments need the ARQ seam below.

Which stage is mandatory: **the PSK is mandatory, ML-KEM is optional** and off by
default. Why it is not a Noise handshake pattern: Noise revision 34 contains nothing
on post-quantum use (verified), and the Noise wiki lists "Hybrid Forward Secrecy" only
as an extension spec "in progress" and the older post-quantum pages (NewHope, SIDH)
under "Old Documents" (SIDH is broken). A staged exchange after the handshake avoids
depending on a non-final pattern. Crate status: `ml-kem` (RustCrypto) is at 0.3.2,
and its README states it "has never been independently audited" (verified). It is not
on the AGENTS.md allowlist. Signal's PQXDH, the nearest deployed analogue, states it
gives no protection against *active* quantum attackers at session time (verified): the
same limit applies here, and the PSK is what covers it.

### 3. Transport records

Per-direction keys come from the Noise split. One record per packet.

| Field | Size | Protected |
|---|---|---|
| type/version | 1 | no (clear) |
| session index | 1 | no (clear; lookup only) |
| counter (little-endian) | 4 | implicitly: it is the AEAD nonce, a wrong value fails the tag |
| inner type/flags | 1 | encrypted |
| channel (shell) id | 1 | encrypted |
| application bytes | n | encrypted |
| tag | 16 | never truncated |

Fixed overhead **24 bytes** (6 clear + 2 inner + 16 tag). The Noise nonce is 64 bits
(ChaCha20-Poly1305: 32 zero bits then the little-endian counter, verified); we send the
low 32 bits and cap the session at 2^32 records, far above any rekey point.

**Payload left for application bytes** (before the ARQ header, which comes out of it):

| Path | Packet ceiling | Application bytes |
|---|---|---|
| A. raw/custom | 184 | **160** |
| B. text, companion carries binary | 160 (companion cap) | **136** |
| B. text, UTF-8-safe encoding (Base64, 4/3 expansion) | 160 chars = 120 B | **96** |

Under B the over-the-air payload is 2 hash bytes + 2 MAC bytes + AES blocks over
`5 + body`, padded to 16: a 160-byte body gives 180 bytes, inside the 184 ceiling. The
text path's 5-byte timestamp/flags overhead is added by firmware and not charged against
the companion cap in this arithmetic; whether it is, is unmeasured. A keystroke echo is 9 B and one
keystroke is a few bytes, so every interactive packet is overhead-dominated (24 B of
overhead on a 3-byte keystroke).

**Replay.** A sliding window over the counter, **64 records** (judgement call: at the
planned rate of at most a few packets per second that is tens of seconds of history,
well beyond the reordering a flood-routed mesh produces; larger costs only memory).
Order of checks on receipt: session index, counter within window and not seen, AEAD
open; the window advances **only after the tag verifies**, so a forged packet cannot
move it. A retransmission is a *new* record with a new counter; duplicates are removed
by the application sequence number (ARQ), not by resending ciphertext.

**Key and nonce safety.** Every handshake contributes fresh ephemeral keys from both
sides, so transport keys are new each time and the counter restarts at zero under a
new key. A restart therefore cannot reuse a (key, nonce) pair, and **no persistent
counter is needed**. Session keys are never written to disk; a restart means a new
handshake (2 packets). The one exposure is a repeated ephemeral from a cloned
virtual-machine snapshot or a broken RNG, which needs both sides to repeat. No random
96-bit nonce is ever used under a long-lived key.

**Rekey.** Changed from the brief: instead of Noise `REKEY` (which `snow` exposes as
`rekey_outgoing`/`rekey_incoming`, and which keeps the nonce running), a **new
handshake** is run after 2^16 records or 12 hours (judgement calls) and on every
reconnect. Reason: REKEY on an explicit-counter, reordering link needs both sides to
hold two keys across a boundary the receiver cannot see exactly; a new handshake costs
two packets, restores real forward secrecy with new ephemerals, and the old session
keeps working until message 2 arrives.

### 4. Metadata and traffic analysis

**Exposed necessarily:** MeshCore header byte (route type, payload type: raw/custom
stands out from chat traffic), path bytes (repeater hash prefixes of 1 byte each, up to
64; the path-length byte also encodes a hash-size code), transport codes if used, packet
size, timing, and under B the 1-byte destination/source hashes of the radios. Under A
there is no destination field: addressing, and what repeaters do with an unaddressed
raw packet, is unmeasured. Flood routing also means every repeater in range
retransmits every ldmux packet: an airtime cost on other users that this design does
not hide.

**What padding can hide:** record sizes, which otherwise reveal key counts and output
volume. Default (judgement call): pad every record to a bucket of **64, 128 or the
packet ceiling** of total payload. Cost on a 3-byte keystroke: 30 B becomes 64 B on
air, about 72 ms to about 113 ms at SF7/125 kHz (computed, see above), and about
doubling at slow presets. Padding is inside the ciphertext; the tag covers it.

**What it cannot hide:** the existence of a session, its start and end, total volume
within a bucket, and gross timing.

**Keystroke timing** is itself sensitive for a terminal: inter-keystroke intervals leak
typed content (the SSH keystroke-timing attacks, Song, Wagner and Tian, 2001; cited
from memory, **unverified**). Mitigations, in increasing cost:

| Option | Hides | Latency cost |
|---|---|---|
| Line mode (`MESH-TRANSPORT.md`): one uplink per command | per-key timing; leaks command length bucket and timing | none beyond line mode |
| Quantise per-key sends to a tick (default 500 ms if per-key mode is used) | intervals finer than the tick | 0-500 ms, mean 250 ms |
| Constant-rate cover traffic | timing almost entirely | bounded by the tick; costs continuous airtime; unsuitable for duty-cycle regions |

Default: line mode, no cover traffic; the tick is offered for per-key mode.

### 5. Integrity of the input direction

A forged keystroke is command execution, so the input path is held to a higher bar than
secrecy:

- **Authenticate before act.** The gateway delivers nothing to the daemon until the tag
  verifies and the counter is fresh.
- **Cheap drop.** Order of checks above puts index, window and handshake filter-tag
  tests before any asymmetric or AEAD work; a forged packet costs at most one
  ChaCha20-Poly1305 verification.
- **Bound to the shell.** The shell id is inside the authenticated plaintext, and the
  session is established by a handshake that proves PSK possession, so a record cannot
  be redirected to another shell or session. Shell attach/claim stays the daemon's
  ownership model (ADR-0010) through its Unix control socket; the gateway holds exactly
  a paired phone's authority and should be confined as such.
- **ARQ above AEAD.** Acknowledgement, ordering, fragmentation, duplicate removal and
  retransmission are unspecified here and belong in a layer above the record, using
  the inner type/flags byte. The seam is: records deliver authenticated byte strings
  to ARQ; ARQ never sees unauthenticated data and the record layer never retransmits.
  This ADR does not design it.
- **Anti-rollback.** Handshake timestamp monotonicity per pairing (persisted); no
  session state is persisted, so there is no state to roll back.

### 6. Key management and pairing

| Secret | Where | Notes |
|---|---|---|
| Gateway static key | Gateway state dir, mode 0600, directory 0700 | Same trust as the existing bearer token (`crates/daemon/src/auth.rs` creates it 0600 from the first byte and re-asserts the mode). |
| PSK, per pairing | Gateway state dir, 0600; phone | One per phone, so one phone can be revoked. |
| Phone static key | Phone, generated at pairing | The gateway **pins** it at the first successful handshake: a different static presenting the same PSK is refused. This is trust on first use on the gateway side only, gated by a 256-bit secret that only the QR holder has. |

**Phone storage.** The phone should hold the static key and PSK wrapped by a
non-exportable Android Keystore key (the wrapping mechanism is the safe assumption;
native X25519 in Keystore is **unverified**). The current app stores the bearer token
in plain preferences; that is a known weakness being tracked and this design must not
inherit it.

**Revocation.** Re-pairing a phone issues a new PSK and a new phone static key and
deletes the old pairing record. Rotating the gateway static key invalidates every
pairing. Compare ADR-0017: the bearer token is shared and unpair is not revocation;
a per-pairing PSK is the per-device revocation that ADR named as follow-up for the
mesh path only.

**QR additions** (version 2 of the link, same `ldmux://pair` scheme): gateway static
public key (32 B, 43 characters Base64url), PSK (32 B, 43 characters), and the gateway's
MeshCore address material if carriage B or a directed path is used (32 B, 43
characters), plus a profile field. The current link is about 85 characters; the
additions bring it to roughly 230. The encoder (`crates/daemon/src/qr.rs`) supports
versions 1-40 at every level; a link this size needs about version 9-10, from
the standard capacity table recalled from memory, **unverified**. The tailnet host
restriction in ADR-0017 does not apply to mesh pairing. As ADR-0017 notes for the token,
anyone who photographs the code holds the PSK.

**Attacker outcomes.**

| Attacker | Learns / can do |
|---|---|
| Sniffs the radio | Ciphertext and the metadata in section 4. Cannot decrypt, forge or replay. A recording adversary with a future quantum computer and no PSK learns the phone's static public key from message 1, nothing else. |
| Steals the phone | Static key and PSK (unless wrapped and device locked): can impersonate that phone until revoked. Past sessions stay safe: each used ephemeral keys that are gone. |
| Steals the PC | Gateway static key, all PSKs, and the shells themselves. Past recorded sessions stay safe classically (ephemeral keys). The host compromise itself already exceeds what the radio layer protects. |
| Jams or drops | Denial of service; out of scope. |

### 7. Implementation guidance (for later)

- Use a maintained Noise implementation, **`snow`** (0.10.0, last released 2025-07-19)
  with its ring resolver, rather than hand-rolling the protocol. **`snow` is not on the
  AGENTS.md dependency allowlist: adding it needs a decision then.** Its README says it
  "has not received any formal audit" and implements revision 34 without the `fallback`
  modifier. Open: whether its transport API allows the explicit-counter, windowed
  replay handling above (it has `set_receiving_nonce`, "useful for lossy transports",
  verified; whether a failed open leaves the counter untouched is **unverified**). If it does not,
  the record layer is a thin wrapper over ring's ChaCha20-Poly1305 keyed from the
  handshake, which still needs a decision about how to extract the split keys.
- Known-answer vectors from the Noise test suite for the exact suite, checked in.
- Fuzz the packet parser and the handshake parser.
- No custom primitives. The filter tag uses HMAC-SHA-256 as specified, nothing new.
- Constant-time comparison for tags and keys; zeroize key material on drop and on
  rekey; never log it.
- Per the Noise spec, the PSK is used only with SHA-256 and only inside Noise (the
  filter-tag key is derived from it, not the PSK itself).
- Pin the cross-component contract with an end-to-end test in the same commit
  (AGENTS.md rule), against a stub transport, with the stub proven to have run.

## Alternatives considered

- **Rely on MeshCore's crypto (DM or channel).** Rejected: ECB, 2-byte MAC, no forward
  secrecy, guessable hashtag keys, and the author would not carry secrets over it.
- **Static symmetric key with an AEAD, no handshake.** Rejected: no forward secrecy, and
  counter state would have to be persistent to avoid nonce reuse after a restart.
- **Noise XX or NK.** Rejected: XX is trust-on-first-use; NK does not authenticate
  the phone. IK with the QR-delivered key gives mutual authentication.
- **PSK-less IK.** Rejected: the user requirement is above-consumer-grade; IK alone
  falls to Shor.
- **AES-256-GCM.** Not rejected on strength: ring has it; rejected as the default because it
  gains nothing here, assumes AES hardware for speed on small devices, and a
  repeated nonce is catastrophic. ChaCha20-Poly1305 has the same nonce rule; the
  counter design is what prevents it.
- **XChaCha20-Poly1305 with random nonces.** Random nonces are safe with its 192-bit
  nonce, but the nonce costs 24 B per packet against 4 B here, and ring's support is
  **unverified**. Not nonce-misuse resistant either.
- **AES-GCM-SIV (misuse resistant).** Not in the Noise suite; the 16-byte tag and counter
  design already avoid misuse; ring support **unverified**.
- **Post-quantum KEM in the mandatory handshake.** Rejected: 2,272 bytes of KEM
  material is 15 packets before the first byte of data, the pattern is not final, and
  the crate is unaudited. Kept as an optional stage.
- **Signal-style double ratchet.** Rejected: a per-message DH step and persistent state
  for a link with a few packets per second and a shell lifetime of hours.
- **Truncated tag to save bytes.** Rejected: 16 of 184 bytes is the cheapest part of
  the budget to keep.

## Consequences

- **+** The mesh and its repeaters see only ciphertext and the metadata in section 4.
- **+** Classical security is Noise IK with forward secrecy; quantum exposure is
  limited to PSK-plus-recordings, and an optional ML-KEM stage covers that.
- **+** MeshCore's weak layer is not load-bearing; if carriage A works it is absent.
- **−** 24 bytes per packet; 160 bytes of application payload in the best case, 96 in
  the worst.
- **−** A second secret (PSK) to provision, store and rotate; the QR carries it, so
  the QR is as sensitive as the token (ADR-0017).
- **−** A new dependency decision (`snow`; `ml-kem` if the optional stage is built) and
  an unmeasured transport.
- **−** Padding and tick quantisation cost airtime and latency; cover traffic is
  excluded by default.
- Nothing in the tailnet path changes.

## Open items

- **Measure** whether the companion API can send raw/custom (`0x0F`) packets and
  whether repeaters forward them; and whether it carries arbitrary binary in text
  messages. Until then carriage A and the 160/136 figures are assumptions.
- Whether the 160-byte companion cap excludes the 5-byte text overhead.
- Addressing for carriage A (it has no destination field), and the airtime burden of
  flooding unaddressed packets.
- ARQ, ordering, fragmentation and dedupe: a separate design (seam in section 5).
- Whether `snow` exposes enough of the transport state; the `snow` allowlist decision.
- Reordering and loss on a real mesh: the 64-record window, 12-hour/2^16 rekey point,
  5-minute timestamp window and 64/128/ceiling buckets are judgement calls to revisit
  with measurements.
- Airtime figures: confirm with real radios.
- Android Keystore: native X25519 support (unverified) and the migration of the
  plaintext-preferences token.
- Per-pairing PSK for the WebSocket path (ADR-0017's per-device token follow-up) is
  separate and not decided here.
- Cross-reference: payload compression, if used, would sit **above** the record layer,
  and compressing before encrypting leaks length per input; see ADR-0019 before enabling
  it on this link.

## Update, 2026-10-28: figures this ADR stated that the firmware and the code have since settled

The text above is left as written. Where it is now wrong, the correction is here, with its
evidence.

- **CORRECTION: the raw/custom payload is not 184 bytes, and application bytes are not 160.**
  The table in section 3 (`A. raw/custom | 184 | 160`) takes MeshCore's 184-byte packet
  ceiling. The companion command that sends raw data is `[0x19][path_len][path][payload]`
  inside a 176-byte companion frame, which accepts `174 - path_len` payload bytes
  (`MyMesh.cpp:1515-1530`, `BaseSerialInterface.h:5`). At the carrier's ceiling a record is
  **172** bytes, and after the 24-byte overhead there are **148** application bytes, of which
  the ARQ header takes 4 (`crates/session/src/record.rs`, `arq.rs`). The B rows (136, 96) are
  unaffected by this but stay unmeasured.
- **ANSWERED: "whether the companion API can send raw/custom (0x0F) packets".** It can: the
  command above exists in the pinned firmware. What stays unmeasured is a raw record crossing a
  real mesh to a real peer; the live run of 2026-10-28 reached the radio and read its contact
  table but did not send an uplink record.
- **ANSWERED: "whether a failed open leaves the counter untouched".** `snow`'s
  `StatelessTransportState` takes the nonce as an argument and holds no counter, so a failed
  open cannot disturb one (`src/stateless_transportstate.rs`, `read_message(&self, nonce, ...)`).
- **STALE: the ring reasoning in section 2.** "SHA-256 rather than BLAKE2s because snow's ring
  resolver offers SHA-256/512 but not BLAKE2" is true of the ring resolver, but snow's default
  RustCrypto resolver offers SHA256 and is what ADR-0023 uses. The suite is unchanged.
- **Implemented:** handshake message sizes 121 B and 50 B match the table in section 2 exactly
  (`handshake_messages_are_the_adr_sizes`), and the record layout of section 3 is
  `record.rs`. Not implemented: padding (section 4), the QR additions (section 6), the optional
  ML-KEM stage, and acting on the rekey trigger.
- **Not in this ADR, decided in code:** the ARQ (`arq.rs`). Its defaults are judgement calls.
