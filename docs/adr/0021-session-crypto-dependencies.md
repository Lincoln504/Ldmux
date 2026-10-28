# ADR-0021: The L3 session's crypto dependencies (`snow`, `hmac`, `sha2`, `zeroize`)

**Status:** Accepted (2026-10-28). Implements the dependency decision ADR-0020 deferred
("`snow` is not on the AGENTS.md dependency allowlist: adding it needs a decision then").

- **Related:** ADR-0020 (mesh link security), `docs/MESH-GATEWAY.md`, `docs/STATE.md`
- **Scope:** `crates/session` only. Nothing else in the workspace depends on these crates.

## Context

ADR-0020 specifies `Noise_IKpsk2_25519_ChaChaPoly_SHA256`. The project rule is not to hand-roll
cryptography, and `docs/STATE.md` listed the choice of a Noise implementation as the first
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
RustCrypto one, not ring. snow's `std` feature turns `ring` on (`std = [..., "ring/std", ...]`),
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
  `MESH_SESSION_BUILT` in `packages/mobile/.../mesh/MeshRoute.kt` stays `false` and the route
  reports itself unfinished.

## Consequences

- **+** No hand-rolled crypto; the suite is checked against an independent vector.
- **+** Four direct dependencies in one crate; no TLS or HTTP stack, no C.
- **+** The pure layering (`crates/session` has no I/O) keeps the swap contained.
- **−** A new unaudited-library risk the ADR names instead of burying.
- **−** The phone has no implementation yet.
