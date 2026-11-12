# ADR-0039: Unified Rust radio core and batch transaction architecture

**Status:** Proposed / Plan of record (2026-11-11). Revises ADR-0025, ADR-0026, ADR-0032, ADR-0034, ADR-0035, ADR-0037, and ADR-0038.
**Specification:** [`docs/RADIO-PLAN.md`](../RADIO-PLAN.md)
**Evidence:** [`docs/RADIO-AUDIT-REVIEW.md`](../RADIO-AUDIT-REVIEW.md)

## Context

The initial radio stack grew to ~17,000 lines across Rust, C-FFI, Kotlin, and Java by adapting a 50 ms continuous live-streaming bridge (`mesh_bridge.rs`) and a byte-stream ARQ (`arq.rs`) to a half-duplex multi-hop MeshCore LoRa link while duplicating companion framing, routing, and terminal wire protocols across four languages.

Field testing at 14–34 hops and full source inspection (`docs/RADIO-AUDIT-REVIEW.md` `RC-1`..`RC-12` and `IC-1`..`IC-8`) proved five structural failures:

1. **Inverted `Session` / `Carrier` boundary**: `Session::poll_with_seq` sealed records to a scalar `record_limit` before the carrier chose Direct (`RAW_CUSTOM`, up to `min(172 − P_out, 171)` B) vs Flood (`LDMR`, `<= 161 − S − P_arrival` B at the companion's `0x88` 173-byte receive-log filter), dropping flooded records and corrupting byte streams via `requeue_remainder`.
2. **Duplicated 4-language protocol stack**: Drawing the Android JNI boundary around `Handshake` + `Session` alone left 8,000+ lines of companion command queues (`MeshcoreSession.java`), routing (`RadioRoute.kt`, `AndroidRadioLink.kt`), and terminal codecs (`MeshTerm.kt`, `MeshTermDriver.kt`) duplicated in Java/Kotlin — causing BLE command-window collisions (`RC-2`), double-wrapped `LDRR` replies (`RC-4`), handshake ephemeral-key churn (`RC-5`), and dead flood token buckets (`RC-10`).
3. **50 ms live streaming & `Hold` over a batch radio link**: Streaming `Profile` + `Rows` + `Commit` from a 50 ms ticker across a lossy `Exchange` ring required `Message::Hold` to freeze output, which cancelled resize repaints (`owe_frame = false`, `RC-11`) and prevented ACK piggybacking (`IC-3`).
4. **Byte-stream `Arq` over `StatelessTransportState`**: A single lost fragment of a stale screen blocked newer screens, mid-burst `Ack` timers collided with oncoming downlink records at repeaters, and retry exhaustion destroyed the `StatelessTransportState` Noise session (`RC-7`).
5. **Forked mobile UI & split device identity**: Forking `MeshTerminalScreen.kt` from `AppContent.kt` and claiming radio shells under `"mesh-link"` instead of the phone's identity caused cross-shell screen bleed (`S7`), `CLAIM_RELEASE_MS` wedges (`S14`), and Tailnet ↔ Radio self-lockouts (`IC-1`, `S6`).

## Decision

Execute five architectural transformations (`A1`–`A5` in `docs/RADIO-PLAN.md`):

1. **A1 · Unified `RadioEndpoint` (`crates/mesh` + `crates/session`)**: One pure-Rust state machine owns `CompanionFifo` (strict 1-command-at-a-time serialization), `RouteTable`, per-attempt carrier selection (`carrier_for(route, attempt)`), 1-round-trip bootstrap fragmentation (`KIND_MSG1_FRAG = 0x14`), idempotent handshake cache + `tx_nonce`, direction-based self-echo rejection (`KIND_UP = 0x13` vs `KIND_DOWN = 0x16` and `hops == 0`), and post-authentication route updates.
2. **A2 · Shared Rust radio core on both Host and Phone (`crates/session-ffi`)**: Move the JNI boundary up so both host and phone run the exact same Rust `CompanionFifo`, `RadioEndpoint`, and `mesh_term` engine (`libldmux_session.so`, zero `tokio`/`nix`), reducing Android BLE to a ~200-line frame pipe (`AndroidBlePipe.kt`) and enabling full-stack `VirtualMesh` tests in `cargo test`.
3. **A3 · Demand-Driven `MeshRpcService` & Single-Burst `3.5 s` Settle-or-Midstate Rule**: Replace the 50 ms `mesh_bridge.rs` polling loop and `Hold` message with a single-task RPC service driven by `PaneStream`'s `eventfd` output notification. Each `Send` coalesces all draft items (`Text`, `Paste`, `Keys`) into one uplink burst and produces **at most one** downlink `Snapshot` burst (`settled = 1` if quiet for `500 ms` after output or `700 ms` with no output; `settled = 0` mid-state at `3.5 s`).
4. **A4 · Message-Oriented Reliable Transaction Layer over persistent `StatelessTransportState`**: Replace byte-stream `Arq` + `TermDecoder` with `(tx_id, part_idx, part_count, ack_tx_id, ack_mask)`, holding receiver `Ack`s until `part_idx == part_count - 1`, sub-splitting `> 63 B` direct parts into `SUB_A`/`SUB_B` on flood retry, superseding stale snapshots cleanly, and keeping `StatelessTransportState` alive across transaction timeouts (using PSK-authenticated `KIND_NEED_HANDSHAKE = 0x15` when a restarted host has no session).
5. **A5 · Unified Mobile Pager, Viewport Fidelity & Cryptographic Device Identity**: Unify `AppContent.kt` and `MeshTerminalScreen.kt` onto one 5-shell pager with per-shell `MeshPageController` state, non-scrolling `⏎` provisional draft echo, keyboard-up grid gating, cursor-aware `GridPlacement`, and unified `device.id = hex(phone_static_pub[..8])` ownership across Tailnet and Radio.

## Consequences

- Deletes ~15,165 lines (~90%) of duplicated Java/Kotlin/bridge radio code (`RADIO-PLAN.md` §8.5).
- Guarantees `<= 173 B` receiver `0x88` passage at all legal hop counts (0..63) and hash widths (1B/2B/3B).
- Eliminates self-jamming precursor floods, BLE command-window races, repeater self-echoes, mid-burst ACK collisions, blank clipped screens, and Tailnet ↔ Radio ownership lockouts.
