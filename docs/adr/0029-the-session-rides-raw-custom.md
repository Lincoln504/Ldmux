# ADR-0029 — Our session layer rides MeshCore's `RAW_CUSTOM` payload

**Status:** Accepted (2026-11-06).

- **Date:** 2026-11-06
- **Related:** `crates/session/` (the session), `crates/companion/src/codec.rs`
  (`RAW_CUSTOM_DIRECT_HEADER`, `build_raw_direct`), `crates/daemon/src/mesh_serial.rs`,
  `docs/ARCHITECTURE.md` §4, §7, §9, ADR-0020 (the mesh session), ADR-0023 (the crypto
  dependencies)

## Context

MeshCore carries several payload types. Two of them could carry a terminal session: the group
channel payloads (`GRP_TXT` 0x05, `GRP_DATA` 0x06), which encrypt with a channel-wide secret,
and `RAW_CUSTOM` (0x0F), which the firmware documents as "custom packet as raw bytes, for
applications with custom encryption, payloads, etc."

The group-channel envelope, read from the firmware rather than from its documentation
(`Utils.cpp`, `MeshCore.h`, and issue #259), is:

- AES-128 in **ECB** mode, no IV or nonce anywhere in the encryption path, zero padding;
- a HMAC-SHA256 tag truncated to **two bytes**, compared with a plain `memcmp`;
- one shared secret per channel, used directly as both the AES key (first 16 bytes) and the
  HMAC key (all 32), with no KDF and therefore no key separation;
- a 1-byte channel hash, so a receiver trial-decrypts up to four candidate channels;
- for the hashtag channels in practice, a key derived from the channel *name*
  (`sha256("#name")[:16]`), which is public knowledge to anyone who types the same name;
- and per-contact direct traffic under a static-static ECDH secret with no rekey: the
  maintainer's own words in issue #259 are "right now we never rekey at all", which is no
  forward secrecy and no post-compromise security.

For messages typed into a terminal, every one of those is a property we cannot accept: ECB
means identical plaintext blocks produce identical ciphertext, a 16-bit tag is forgeable in
1 in 65,536 attempts per try, and a shared channel key means any member of the channel can
forge a sender — the firmware itself labels the group types "unverified".

MeshCore's own transport is also the wrong shape for a screen: it has no ordering, no
acknowledgement of application data, no replay window and no fragmentation. A terminal needs
a reliable, ordered, replay-protected byte stream.

## Decision

1. **The session is ours, and it travels in `RAW_CUSTOM`.** A record is a `Noise_IKpsk2`
   sealed message (`Noise_IKpsk2_25519_ChaChaPoly_SHA256` over `snow`), packed into a
   `RAW_CUSTOM` direct packet addressed to the peer radio's public key
   (`RAW_CUSTOM_DIRECT_HEADER = 0x3E`). MeshCore forwards the packet by route and never reads
   its payload, which is exactly what the payload type is for.
2. **No MeshCore channel key is involved.** The preset the two radios share is a frequency,
   not a secret: the confidentiality comes from the pairing PSK and the handshake, and the
   radio's own encryption is never applied to our bytes.
3. **Reliability is ours too.** The ARQ in `crates/session/src/arq.rs` provides ordering,
   acknowledgement, retransmission, a replay window and the airtime budget; MeshCore provides
   only the hop.
4. **We pay for it in bytes, not in correctness.** 24 bytes of overhead per record (6 clear,
   2 inner, 16 tag) leaves 148 application bytes of a 172-byte record; a handshake is 121 + 50
   bytes, two records each way on a slow preset.

## Consequences

- **What it buys**: a nonce that never repeats (the record counter, with a fresh one per
  retransmission and `u32::MAX` refused), a full-length tag, separate keys per direction (so
  the half-duplex link cannot reuse a key and nonce across directions), a replay window,
  forward secrecy per session from ephemeral keys, and rekey by a fresh handshake rather than
  `Rekey()` — which would keep the nonce while replacing the key.
- **What it does not buy**: no ratchet, so no post-compromise security; no padding, so record
  sizes follow the text; and nothing hides the L2 path, the node hashes or the traffic volume,
  which MeshCore broadcasts in plaintext by design.
- **A leaked pairing code is a leaked session.** The PSK and the bearer token travel in the
  same QR code, so a photograph of it is enough. That is a property of pairing, not of this
  decision.
- **The firmware's own limiter is not the legal one.** Its budget factor is 1.0, a 50 % duty
  cycle, so the host computes and enforces the 1 % allowance itself (`docs/ARCHITECTURE.md` §9).
- **Rejected: a MeshCore group channel as the carrier.** It would have saved the handshake and
  the framing, and cost us ECB determinism, a 16-bit tag, a channel-wide key that any member
  can forge with, and a hashtag key anyone can derive from the channel name.
- **Rejected: plaintext `RAW_CUSTOM` with no session layer.** It would remove the handshake,
  and it would put every keystroke and every line of screen text on the air in the clear.
- **Rejected: TLS or DTLS over the radio.** `deny.toml` bans TLS stacks, and a TLS record
  header alone is larger than the payload budget this link has; Noise was designed for this
  shape of transport.
