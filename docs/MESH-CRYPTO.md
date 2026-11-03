# The mesh session's cryptography, audited against the Noise framework and RFC 8439

Written 2026-11-03. This is the review of what `crates/session` does, why it does it, how it
compares with what MeshCore itself provides, and what is still missing. ADR-0023 records the
decision; this document records the audit of the decision and of the code that implements it.

## Why there is an app-level session at all

MeshCore's own direct-message encryption is weaker than it looks, and the primary and secondary
records now agree on the details:

- A node's identity is Ed25519; the pairwise secret is `ed25519_key_exchange` over those keys,
  computed once and cached for the life of the contact (MeshCore spec §14-15, firmware
  `ContactInfo::getSharedSecret`). There is no ephemeral contribution, so no forward secrecy.
- Payloads are AES-128 in **ECB** with zero padding, then HMAC-SHA256 truncated to **two bytes**
  (spec §14, firmware `encryptThenMAC` / `MACThenDecrypt`). No nonce, no KDF, no direction
  separation, roughly 1-in-65,536 per-attempt forgery, and `memcmp` for the MAC comparison
  (`Utils.cpp`), which is not constant time.
- Group messages carry no sender signature at all, and routing hashes are one byte
  (`PATH_HASH_SIZE = 1`, 256 values by design).
- A disclosure in April 2026 documented a remotely triggerable heap overflow in the
  `PAYLOAD_TYPE_PATH` handler, fixed in v1.14.0 with no advisory, and put the AES-ECB and 2-byte
  MAC facts on the record.

So a terminal session cannot rely on it: no forward secrecy, a 16-bit MAC on a link where every
frame is a forgery oracle, and no key confirmation. `crates/session` runs Noise over the carrier
instead, and MeshCore becomes a transport that may be read but not trusted.

## What the session is

- **Pattern: `Noise_IKpsk2_25519_ChaChaPoly_SHA256`.** The phone initiates, the host is the
  responder, the PSK comes from the pairing code, and the host pins the phone's static key on the
  first accepted handshake.
- **Transport: `snow`'s `StatelessTransportState`.** The counter is carried in the record and
  owned by the ARQ, not by the library — the stateful transport state would diverge from the wire
  on the first lost record. This is exactly the distinction `snow` documents for unreliable
  transports.
- **Record: 6 clear + 2 inner + 16 tag.** The 4-byte little-endian counter is the AEAD nonce, the
  session index selects the key, and the inner flags and shell id are encrypted. At the carrier's
  172-byte ceiling that leaves 143 application bytes after the 5-byte ARQ header.

## What the audit found to be right

- **The tag is never truncated.** 16 bytes, as RFC 8439 requires in as many words ("any protocol
  using this algorithm MUST include the complete tag... Tag truncation MUST NOT be done"). The
  temptation on this link is real — an 8-byte tag would buy 8 payload bytes — and the reason not
  to is that truncation turns the failed-authentication budget into a link-quality sensor: an
  attacker gets rapid feedback per guess, and the honest receiver cannot tell noise from attack.
- **The nonce is a 32-bit record counter, not the ARQ's 16-bit sequence.** Two counters that look
  similar and must not be confused: the ARQ's wraps every 65,536 records and is only for
  retransmission bookkeeping, while the record counter is the AEAD nonce and is checked for
  exhaustion (`CounterExhausted` at `u32::MAX`). Using the ARQ sequence as the nonce would be a
  catastrophic reuse bug; the code does not.
- **The replay window is committed only after the tag verifies.** `open` calls `window.check`
  before decrypting and `window.commit` only after `read_message` succeeds, so a forged record
  cannot advance the window or poison the bitmap — the rule DTLS 1.3 states as "the window MUST
  NOT be updated due to a received record until that record has been deprotected successfully."
- **The window is 64 records wide.** With at most eight records in flight, that is eight times
  the depth any reordering on a half-duplex link can produce.
- **Secrets are zeroed.** The handshake's private key and PSK use `zeroize` (`handshake.rs`), and
  `snow` zeroes its own state.
- **Key confirmation happens on first use.** The host does not treat the session as up until it
  has opened a record under it, which is the WireGuard rule and the reason the phone sends an
  attach immediately after the handshake.

## What the audit found missing

1. **The rekey trigger is phone-driven, and that is correct but unstated.** `Session::needs_rekey`
   answers true at 2^16 records or at 12 hours, and the phone's driver starts a new handshake when
   it does. The host has no timer of its own — it is the responder, so it cannot initiate — and a
   fresh `msg1` replaces its session and clears the old outbox, which is what makes the phone's
   timer sufficient. What is missing is the sentence in the code saying so: a reader of
   `mesh_session.rs` finds no mention of rekey at all and has to infer that this is by design.
2. **The clear header is not authenticated.** The counter is the nonce and the index selects the
   key, so a flipped byte fails the tag; but the design is recorded as "lookup only" rather than
   as an explicit decision about the *kind* byte, and `snow`'s transport API takes no associated
   data. Anyone reading the record can also see its length and index.
3. **No cookie/DoS filter on message 1.** `IKpsk2`'s first message is `0,0` — unauthenticated,
   unencrypted, replayable at the Noise layer — and answering it costs two X25519 operations and
   an AEAD decryption. On the radio path that is cheap to abuse only if an attacker is already
   transmitting; on the tailnet it is not reachable at all. Worth stating in the ADR rather than
   leaving implicit.
4. **Enrolment is trust-on-first-use.** The host pins whichever phone completes the first
   handshake with the PSK. That is stronger than MeshCore (which caches a static-static secret
   with no confirmation at all) and weaker than WireGuard (which has a configured list of
   acceptable peers): a holder of the PSK enrols first. The PSK is 32 random bytes from the
   pairing code, so this is a real but bounded exposure, and it is the only enrolment a
   two-device ceremony can do without a second channel.
5. **No padding.** Record lengths are visible on the air. ADR-0020 §4 reserves a flag bit for a
   padded form; nothing sends one.

## Post-quantum, stated plainly so it is not rediscovered later

ML-KEM-768's encapsulation key is 1,184 bytes: seven records at 172 bytes before any ciphertext,
and a hybrid `XXhfs` handshake measured elsewhere at 2,480 bytes with empty payloads is fifteen
records. At 5% record loss a fifteen-record handshake completes about 46% of the time, and every
fragment that fails costs a whole handshake again. The published engineering guidance is
explicit that in-band post-quantum handshakes should not run over duty-cycled LoRa uplinks.
The shape that would work: pay the post-quantum cost once, in the pairing ceremony, which is
already out of band and can carry any amount of data; and keep the radio session classical, where
the forward secrecy comes from its ephemerals every twelve hours. Compute is not the obstacle —
ML-KEM-768 encapsulation is about 10 ms on a Cortex-M4 at 64 MHz — bandwidth is.

## The five changes worth making, in order

1. **Write the rekey ownership down** where a reader of the host finds it, and add the test that
   proves a second `msg1` on a live session replaces it cleanly (the behaviour the phone's
   twelve-hour timer depends on).
2. **Write the header decision down**: which bytes are authenticated by construction (counter as
   nonce, index as key selector) and which are not, so the next reader does not assume AAD.
3. **A failed-tag budget.** Count bad tags per session and force a rehandshake after a small
   number, which is what turns a 16-byte tag into a usable integrity signal instead of an
   unbounded oracle.
4. **Padding behind the reserved flag bit**, at least to 16-byte steps, so record lengths stop
   leaking the shape of what is typed.
5. **State TOFU explicitly in the pairing ceremony's text**, because a user who understands that
   the first phone to scan owns the shell will treat the code like the credential it is.
