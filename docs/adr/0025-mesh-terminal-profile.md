# ADR-0025: The mesh terminal profile (plain-text row diffs inside the session)

**Status:** Accepted (2026-10-28). Implements `docs/MESH-GATEWAY.md` section 8 item 2. **Update 2026-10-28 (later):** the phone side is built. `MeshTerm.kt` is the Kotlin port, checked against `crates/protocol/test/fixtures/mesh-term-vectors.json` (written by the Rust encoder); `MeshTermDriver` and `MeshTerminalScreen` are the attach driver and the reduced terminal; `ldmux code --mesh` delivers the keys. The section "Not decided" below named the ceremony that tells the phone which shell to attach: it is still not built, and the phone sends an empty selector (the host answers `Closed(AMBIGUOUS)` if more than one shell runs).

- **Related:** ADR-0018 (the keeper owns the emulator), ADR-0020 (link security, record layout), ADR-0023 (session crates), `docs/MESH-TRANSPORT.md`
- **Scope:** `crates/protocol/src/mesh_term.rs`, `crates/daemon/src/mesh_bridge.rs`, and the phone's port of the same encoding. MeshMore never parses any of it: it travels inside the Noise session.

## Context: the budget

All figures are measured or derived in this repo, not assumed.

| Quantity | Value | Source |
|---|---|---|
| Airtime of a 176-byte frame at the deployed preset (62.5 kHz, SF7, CR5) | 612 ms | `AirtimeEstimator.lora(...).forBytes(176)`, verified 2026-10-28 |
| Airtime the governor allows per hour | 36 s | design as if EU868 1 % applied (MESH-GATEWAY section 6) |
| Records per hour | 36 / 0.612 = **58** | same |
| L2 record | 172 bytes | `crates/mesh/src/constants.rs:24` |
| Session overhead | 6 clear + 2 inner + 16 tag = 24 | ADR-0020, `crates/session/src/record.rs` |
| ARQ header | 4 | `crates/session/src/arq.rs` `HEADER` |
| **Payload per record** | **144** | 172 − 24 − 4 |
| **Bytes per hour, one direction** | 58 × 144 = **8,352** | |

A VT byte stream is tens of KB per screen and `ls -laR` is 514,758 bytes (`docs/MESH-TRANSPORT.md`). It cannot cross this link.

## Decision

**Model (a): the host sends the screen as plain-text rows, and afterwards only the rows that changed.** The source is the keeper's existing `Snapshot` (the `snapshot` RPC: `text`, `cols`, `rows`, `cursor_x/y`, `alternate_on`, `dead`). The daemon strips the SGR escapes and trims trailing spaces (`Screen::from_keeper`) and diffs against what it last sent.

Why this and not the alternatives:

- **(b) filtered PTY bytes plus a render proxy** is a second emulator in the daemon and still has no bound on output volume. Rejected.
- **Cell-level or styled diffs** cost 9 to 25 bytes per cell (the figure `MESH-TRANSPORT.md` first used and then retracted). Colour is the thing a one-record-per-minute link can least afford. Rejected for v1; it is the first thing to add if measurement says there is room, via a new `Rows` flag.
- **Reuse of existing vocabulary.** `snapshot`, `cols`, `rows`, cursor position, `alternate_on` and "the viewer resyncs from a snapshot when output was lost" (`PeekResult.lost`) are the existing concepts, and are used under the same names. **There is no existing row-hash path in the host**: a search of `crates/daemon` for a per-row hash or changed-rows computation finds none (the daemon sends bytes, not rows), so the row comparison here is new and is plain string equality against the last sent screen.
- **Reliable ordered session underneath.** Because the ARQ delivers in order, what was last sent is what the phone holds, so a diff against the last sent screen is sound. A reconnect or a `Resync` resets it to a full screen.

## Message set

Framing: `tag u8 | len (LEB128, 1 or 2 bytes) | body`. The stream is split by the ARQ anywhere; a decoder reassembles. An unknown tag is skipped; a body over 300 bytes ends the attach.

| tag | dir | name | body |
|---|---|---|---|
| 0x01 | phone→host | Attach | version u8, selector 0..=40 bytes |
| 0x02 | phone→host | Input | 1..=96 raw bytes |
| 0x03 | phone→host | Resync | empty |
| 0x04 | phone→host | Detach | empty |
| 0x81 | host→phone | Profile | version u8, cols u8, rows u8, unsupported u16 LE |
| 0x82 | host→phone | Rows | epoch u16 LE, flags u8 (bit0 full), patches |
| 0x83 | host→phone | Commit | epoch u16 LE, cursor_x u8, cursor_y u8, flags u8 |
| 0x84 | host→phone | Closed | reason u8 |

A patch is `y u8 | flags u8 (bit0 append) | n u8 | n UTF-8 bytes`; a row over 120 bytes is several segments. The phone paints on `Commit` only; `Rows` for an epoch are held until its `Commit`.

**Attach.** The selector is a session-id prefix. Empty means "the one running shell"; if there are several the host answers `Closed(AMBIGUOUS)`. Listing shells is not part of the mesh profile: the phone is told which shell at pairing.

**Away for an hour.** Nothing is replayed. The host computes an update only when nothing is queued toward the phone, and it is a diff of the present against what was last sent. Fifty changes while the phone was unreachable are one diff (`a_client_that_missed_many_changes_converges_by_one_diff_against_the_present`).

**Ack and credit.** None at this layer. Delivery, ordering, retransmission and the 8-record window belong to the ARQ, and the airtime bound to the gateway's `recordsLeftThisHour`. The bridge sends an update only when the session has nothing pending, so a slow link produces fewer, newer updates and never a queue of stale ones.

## Arithmetic

- Every host→phone message encodes to at most **133 bytes** (`MAX_MESSAGE` = 1 + 2 + 130), so each fits one 144-byte record payload. Test: `a_tall_screen_fits_the_record_rules` and `every_host_message_is_within_the_bound_for_any_row_content`.
- One changed row of 40 characters plus a cursor move: Rows (3 + 3 + 40 + 3 header) + Commit (7) is about 60 bytes, **one record**. At 58 records an hour that is at most 58 such updates, and fewer once acknowledgements that cannot ride on data are counted. Plan for about **one update a minute**.
- A mostly empty 80x24 screen with a four-line prompt: 2 records or fewer (`an_ordinary_screen_costs_a_handful_of_records`).
- A completely full 200x100 screen is about 20,600 bytes, 150 records. **That does not fit an hour of airtime and no encoding makes it fit.** The profile does not hide this: the governor stalls it and `mesh.status` shows the stall. Full-screen programs are therefore poor on this link by physics, and the `ALTERNATE` flag tells the phone it is looking at one.

## Update, 2026-10-29: List and Sessions (choosing a shell over the link)

The profile could not say which shells exist, so a host running more than one answered `Closed(AMBIGUOUS)` to the phone's empty selector and the attach ended. Two messages are added, both within the existing framing and the 144-byte record rule:

| tag | direction | name | body |
|---|---|---|---|
| 0x05 | phone to host | `List` | empty |
| 0x85 | host to phone | `Sessions` | `flags u8` (bit 0 = last page), then entries `id_len u8, id, label_len u8, label` |

- An `id` is at most 40 bytes (it is the `Attach` selector), a `label` at most 24, both cut at a character boundary. At most 16 shells are listed.
- A page is packed to 130 bytes so it fits one record; a longer list is several `Sessions` messages and only the last has `last` set. **No shells is one empty page with `last` set**, so "none" is an answer and not silence.
- The host attaches nothing on `List`. The phone chooses, and `Attach(id)` with an id from the list is unambiguous.
- Pinned in `mesh-term-vectors.json` (`messages`, `limits`, and `sessionPages`, a 7-shell list spanning records); both codecs are held to it.
- **Phone behaviour, and a deliberate deviation from "request the list when the selector is empty".** The driver still sends `Attach("")` first. The host takes the one shell when there is exactly one (the common case) and costs no extra record on a link that allows about 58 an hour; `Closed(NOT_FOUND)` becomes the `NoShells` state; `Closed(AMBIGUOUS)` is the cue to send `List`. One shell attaches, several show a picker (`Choose`), none is stated. `choose(id)` accepts only an id the host listed. A shell named explicitly that is ambiguous still ends visibly with no picker.

## Not supported, and how the client knows

`Profile.unsupported` is a bitmask the client must render as absent: scrollback, colour and text attributes, hyperlinks, mouse, images, resize (the phone renders the host's geometry; the pane is never resized from the mesh, because the keeper's geometry is canonical, ADR-0018), clipboard. A version-1 host sets all of them. The phone must show "reduced profile" and must not offer a control the mask says is missing.

## Consequences

- The Rust encoding and a Kotlin port are checked against the same pinned byte vectors (`exact_bytes_are_pinned`).
- `Input` is raw bytes so control characters (Ctrl-C) work. The host writes them to the shell through the existing `send` RPC. Input is capped at 96 bytes per message; the phone's own governor paces it.
- Nothing here touches MeshMore: `rg -i 'mesh_term|Rows|Commit' ` in `meshcore_gateway` and `meshcore_host` finds no hit.

## Not decided

Styled rows, a smaller cell-delta form, a per-message checksum (the AEAD tag already covers every record). **Selecting a shell over the link: decided 2026-10-29, above** (this line used to list it, and the ceremony, as undecided).
