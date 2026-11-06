# ADR-0023: The L3 session's crypto dependencies (`snow`, `hmac`, `sha2`, `zeroize`)

**Status:** Accepted (2026-10-28). Implements the dependency decision ADR-0020 deferred
("`snow` is not on the AGENTS.md dependency allowlist: adding it needs a decision then").

- **Related:** ADR-0025 (what runs inside the session), ADR-0020 (mesh link security), `docs/ARCHITECTURE.md#4-the-wire`, `docs/RELEASE-AUDIT.md`
- **Scope:** `crates/session` only. Nothing else in the workspace depends on these crates.

## Context

ADR-0020 specifies `Noise_IKpsk2_25519_ChaChaPoly_SHA256`. The project rule is not to hand-roll
cryptography, and `docs/RELEASE-AUDIT.md` listed the choice of a Noise implementation as the first
missing piece of the L3 session. Facts checked on 2026-10-28:

- No Noise or curve crate was vendored (`ls ~/.cargo/registry/cache` held neither `snow` nor any
  X25519 / ChaCha20-Poly1305 crate before this change).
- crates.io is reachable (`cargo search snow`, `cargo add snow`, `cargo fetch` all succeeded), so
  a network dependency can be locked.
- `snow` 0.10.0 (`Apache-2.0 OR MIT`) lists `NoiseParams` parsing for the exact suite, and its
  `StatelessTransportState` takes the nonce as an argument to `read_message` / `write_message`
  with `&self`: a failed open cannot disturb a counter. ADR-0020 left that as an open question
  ("whether a failed open leaves the counter untouched is unverified"). It is answered: the
  stateless transport has no counter to disturb.
- The Cacophony known-answer vector for this exact suite ships in snow's own test tree
  (`tests/vectors/cacophony.txt`, one `Noise_IKpsk2_25519_ChaChaPoly_SHA256` entry). It is
  checked in at `crates/session/tests/vectors/noise_ikpsk2_cacophony.json` and replayed by
  `the_noise_suite_matches_the_cacophony_known_answer_vector`, which compares snow's two
  handshake messages and the handshake hash byte for byte against a vector snow did not produce.

## Decision

Add, to `crates/session` only:

| Crate | Version | Why |
|---|---|---|
| `snow` | 0.10, `default-features = false`, features `use-chacha20poly1305`, `use-sha2`, `use-curve25519`, `use-getrandom` | The Noise implementation. Pure Rust. |
| `hmac` | 0.12 | The handshake filter tag (ADR-0020 section 2) is HMAC-SHA-256 "as specified, nothing new". |
| `sha2` | 0.10 | HMAC's hash. Already pulled by snow, so this adds no new crate. |
| `zeroize` | 1 | ADR-0020 section 7: wipe key material on drop. Already pulled by snow's cipher stack. |

**Not `ring`.** ADR-0020 chose SHA-256 because "snow's ring resolver offers SHA-256/512 but not
BLAKE2". That reasoning is stale: snow's own default resolver offers `HashChoice::SHA256`
(`src/resolvers/default.rs`). The suite stays `SHA256` as decided, but the resolver is the
RustCrypto one, not ring. snow's `std` feature turns `ring` on (`std = [.., "ring/std",..]`),
which would add a C/assembly build and the Windows import crates; leaving `std` off avoids that,
and the resulting tree has no `ring` (`cargo tree -p ldmux-session | grep -c ring` is 0).

Also: `deny.toml` gains `BSD-3-Clause` in its licence allow-list, because `curve25519-dalek`
and `subtle` carry it. Both are permissive; neither was previously in the lockfile.

### Alternatives considered

| Alternative | Why not |
|---|---|
| Hand-rolled Noise | Forbidden by ADR-0020 and by the rule against custom primitives. |
| `libp2p-noise` | A Noise XX/IK/IX wrapper inside the libp2p stack: wrong pattern set (no `psk2` placement we control) and an enormous dependency graph for one handshake. |
| `noise-protocol` | `no_std` minimal; far less used and less reviewed than snow, with no resolver abstraction. Kept as the fall-back if snow's maintenance lapses. |
| `boringtun` | A WireGuard implementation (`Noise_IKpsk2` with fixed WireGuard framing). Its handshake is not separable from WireGuard's packet formats. |
| `ring` resolver for snow | Adds a C build and platform crates for no algorithm we need (see above). |

### Known limits, recorded rather than hidden

- snow's README states "This library has not received any formal audit" (`README.md:13` of
  snow 0.10.0, read from the vendored source on 2026-10-28; an earlier research summary in this
  project called it audited, which that line contradicts). ADR-0020 dates the 0.10.0 release
  2025-07-19, so the cadence is also slow. This is the standing risk of this
  decision; the swap point is `crates/session/src/handshake.rs` and `record.rs`, which are the
  only files naming snow.
- snow's transport API takes no associated data, so the 6 clear header bytes are not bound into
  the tag. ADR-0020 designs them as "lookup only" and this is consistent with it; the tests
  `tampering_fails_the_tag_and_leaves_the_replay_window_alone` pin the behaviour that matters
  (a flipped counter or index never opens).
- snow cannot derive a public key from a private key, so the gateway's static key file holds both
  halves (`Keypair::from_parts`).
- **The phone side is not solved by this ADR.** snow is a Rust library. The Kotlin Multiplatform
  mobile client has no Noise implementation, and no maintained KMP `commonMain` handshake library
  exists (surveyed 2026-10-28). The options are a Kotlin implementation of the same suite, JNI
  over this crate, or a different suite; each is a product-level decision. Until one is taken,
  `MESH_SESSION_BUILT` in `packages/mobile/../mesh/MeshRoute.kt` stays `false` and the route
  reports itself unfinished.

  **Update 2026-10-28: taken.** The architecture already fixed it (one implementation of the
  suite, no hand-rolled crypto in Kotlin, nothing duplicated per platform), so the phone calls
  this crate: `crates/session-ffi` exposes it as a C ABI (`packages/mobile/native/ldmux_session.h`),
  `ldmux_session_jni.c` is a thin JNI shim, and `MeshSession` (commonMain) drives it. The ABI is
  deliberately small and pull-style: handles are opaque, every output is copied into a caller
  buffer, free nulls the caller's slot, no global state, a panic is a status code. The library
  is a **separate** `libldmux_session.so` rather than part of `libldmux_terminal.so`, because the
  terminal library is shipped prebuilt from the libghostty pipeline and linking a Rust staticlib
  into it would make every session change rebuild libghostty. It is built by
  `packages/mobile/native/build-session.sh host|android` (the staticlib needs no linker, so cargo
  needs no NDK configuration). `MESH_SESSION_BUILT` is now `true` and is combined with whether the
  library loaded on the device. What stays `false` is `MESH_TERMINAL_BUILT`, because carrying a
  shell needs the compact encoding and the bridge, neither of which existed then. (Update 2026-10-28, later: both exist, see ADR-0025, and the flag is `true`.) Cost accepted: the
  mobile build now needs a Rust toolchain for the session library (the JVM tests build the host
  variant themselves, and the ABI test fails loudly, not silently, if that cannot run).

## Consequences

- **+** No hand-rolled crypto; the suite is checked against an independent vector.
- **+** Four direct dependencies in one crate; no TLS or HTTP stack, no C.
- **+** The pure layering (`crates/session` has no I/O) keeps the swap contained.
- **−** A new unaudited-library risk the ADR names instead of burying.
- **−** The phone's implementation is the same Rust code behind an FFI boundary, so the unsafe
  surface is one crate (`crates/session-ffi`, 13 ABI tests) and the mobile build gains a Rust step.

## Review, 2026-11-03

The choice and the code were audited against the Noise Protocol Framework specification and RFC
8439; `docs/ARCHITECTURE.md#7-security` is the record. The pattern stands: `IKpsk2` with the pairing PSK is the
right shape for a pinned two-node link, `NNpsk0`-style admission would be the alternative if
enrolment ever moved onto the wire, and `KK` is rejected because it loses the initiator's ephemeral
and with it any pretence of forward secrecy from the phone's side.

What the review added to this ADR's reasoning: the two-byte MAC and the ECB mode are what MeshCore's
own crypto amounts to, which is why the app-level session exists at all; the 16-byte tag is not
negotiable, so the record's payload ceiling is 143 bytes and padding, not truncation, is the only
lever on length; and the twelve-hour rekey rule is owned by the phone because it is the initiator,
with the host's job being to replace a live session when a fresh `msg1` arrives (now tested).
