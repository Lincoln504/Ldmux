# ADR-0025: The mesh terminal profile (plain-text row diffs inside the session)

**Status:** Accepted (2026-10-28). Implements `docs/MESH-GATEWAY.md` section 8 item 2. **Update 2026-10-28 (later):** the phone side is built. `MeshTerm.kt` is the Kotlin port, checked against `crates/protocol/test/fixtures/mesh-term-vectors.json` (written by the Rust encoder); `MeshTermDriver` and `MeshTerminalScreen` are the attach driver and the reduced terminal; `ldmux code --mesh` delivers the keys. The section "Not decided" below named the ceremony that tells the phone which shell to attach: it is built (2026-10-29) as the `List`/`Sessions` profile and the phone's shell picker. A host with more than one shell no longer answers `Closed(AMBIGUOUS)` unless the phone sends an empty selector.

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

## Update, 2026-10-30: profile version 2, `Key` and a named shell

Two gaps were found on the phone, and one duplicated table.

- **Keys carried no terminal mode.** `Input` is raw bytes, so the phone had to build the bytes of an arrow or an Escape itself, from a copy of the daemon's key table. It cannot see the pane's cursor-key mode (DECCKM) or Kitty flags, so an arrow was always `ESC [ A` (wrong in `vim`, `less`, `htop`, which expect `ESC O A`) and Shift+Enter was a plain CR. The tailnet path never had the problem: it sends key **names** and the daemon encodes them for the pane (`send` with `keys`, `key_name_to_bytes_in_mode`, `crates/daemon/src/stream.rs`).
- **`Profile` did not name the shell.** After `Attach("")` the host chose the shell, so the phone's header could not mark which one it was on until the user picked one.

| tag | direction | name | body |
|---|---|---|---|
| 0x06 | phone to host | `Key` | 1..=16 key names, one space between, at most 96 bytes in all |
| 0x81 | host to phone | `Profile` | as before, then from version 2 `shell_len u8, shell id` |

- **`Key`.** Names are printable ASCII with no space (`Up`, `S-Tab`, `C-c`, `M-C-S-Left`, `Enter`, `PgUp`). More than 16 names, a body over `MAX_INPUT`, an empty name or any other byte is malformed and costs that message only. The host sends them through the daemon's own `send` RPC, which reads the pane's `app_cursor` and Kitty flags from the keeper and encodes with `key_name_to_bytes_in_mode`. If the mode cannot be read the normal-mode encoding is used, as on the tailnet. A name the table does not know is refused there and nothing reaches the shell.
- **The phone has no key table.** `MeshKeys.kt` is deleted. The shared key bar already emits the names. Typed text still goes as `Input` bytes. The phone's batcher keeps text and keys in the order typed and sends them as separate records.
- **Version 2, no flag day.** `VERSION` is 2 and `MIN_VERSION` is 1. A host accepts `Attach` with either and answers in the version asked for: a version-1 phone gets the five-byte `Profile` it can decode and its `Key` messages are ignored. A version-2 phone against a version-1 host gets `Closed(VERSION)`, which says so, rather than sending a message that host would skip as an unknown tag. The decoder accepts a five-byte `Profile` as version 1 with no shell; a longer body must be version 2 and carry a consistent shell length.
- **The shell id.** The host puts the attached shell's id in every version-2 `Profile`. The phone marks it current at once; the list is still how it learns the other shells.
- **Cursor.** `Commit` already carried the cursor cell. The phone now draws a block there, rebuilding only the cursor's row, at most once per `Commit`.
- Tests: protocol (`key_names_are_one_space_separated_body`, `a_key_with_a_space_too_many_names_or_too_long_a_body_is_refused`, `a_version_one_profile_decodes_with_no_shell_and_version_two_carries_it`, `a_differ_names_its_shell_in_the_attach_version`), bridge (`a_key_message_reaches_the_attached_shell_as_names_never_as_bytes`, `a_key_before_attach_or_from_a_version_one_phone_is_ignored`, `the_profile_names_the_attached_shell_in_the_version_asked_for`), and against a real keeper `crates/daemon/tests/mesh_keys.rs` (`up_is_ss3_when_the_pane_asked_for_application_cursor_keys`, `up_is_csi_in_a_pane_that_did_not`). Both codecs are held to `mesh-term-vectors.json`, regenerated with `LDMUX_REGEN_FIXTURES=1`.

## Not supported, and how the client knows

`Profile.unsupported` is a bitmask the client must render as absent: scrollback, colour and text attributes, hyperlinks, mouse, images, resize (the phone renders the host's geometry; the pane is never resized from the mesh, because the keeper's geometry is canonical, ADR-0018), clipboard. A version-1 host sets all of them. The phone must show "reduced profile" and must not offer a control the mask says is missing.

## Consequences

- The Rust encoding and a Kotlin port are checked against the same pinned byte vectors (`exact_bytes_are_pinned`).
- `Input` is raw bytes for typed text and so control characters (Ctrl-C) work; named keys use `Key` (version 2, above). The host writes them to the shell through the existing `send` RPC. Input is capped at 96 bytes per message; the phone's own governor paces it.
- Nothing here touches MeshMore: `rg -i 'mesh_term|Rows|Commit' ` in `meshcore_gateway` and `meshcore_host` finds no hit.

## Not decided

Styled rows, a smaller cell-delta form, a per-message checksum (the AEAD tag already covers every record). **Selecting a shell over the link: decided 2026-10-29, above** (this line used to list it, and the ceremony, as undecided).

## Update, 2026-10-30 (later): profile version 3 and `Resize`

The mesh terminal clipped on the right. The tailnet route asks the host to resize its pty to the phone (`resize` on the attach stream), so a line is never wider than the screen. This profile had no resize: the host's 80 columns were fixed, and shrinking the phone's font to fit is a workaround that makes the text unreadable on a narrow screen.

| tag | direction | name | body |
|---|---|---|---|
| 0x07 | phone to host | `Resize` | `cols u8`, `rows u8`, each 1..=255 |

- **`Resize`.** A zero in either byte or any other body length is malformed and costs that message only. The host resizes the attached pty through the keeper's own `Resize` message (daemon method `resizePane`, clamped by `clamp_pane_size` exactly as the tailnet's `PaneStream::resize` is), then resets its differ, so the next update is a fresh `Profile` with the new grid and a full screen. A refused resize changes nothing and sends nothing.
- **Version 3.** `VERSION` is 3, `MIN_VERSION` stays 1, `RESIZE_VERSION` is 3. The `Profile` body is unchanged from version 2. A phone that attached below 3 is not obeyed on `Resize`, and a phone that attaches with 3 against an older host is told `Closed(VERSION)`.
- **Shared pty.** As on the tailnet, a pane has one size: resizing it for the phone resizes it for any other client attached to it.
- **Tests.** Protocol: `resize_is_two_bytes_of_cells_and_zero_is_refused_both_ways`, `a_version_three_profile_names_its_shell_like_version_two`, and the regenerated fixture. Bridge: `a_resize_fits_the_pane_and_starts_the_screen_over_with_the_new_grid`, `a_resize_before_attach_from_an_older_phone_or_a_refused_one_changes_nothing`. Real keeper: `a_resize_changes_the_real_pane_and_the_next_profile_says_so` (`crates/daemon/tests/mesh_keys.rs`).

## Update, 2026-10-30 (profile version 4): messages share a record

An echo of one typed line used to cost two 176-byte frames: the `Rows` that carried the
changed line and the `Commit` that paints it, 5 bytes of body. The bridge sent every message
as its own record, although `RECORD_PAYLOAD` is 144 and the phone's decoder (`TermDecoder`)
reassembles messages from an arbitrarily chunked stream.

From version 4 the bridge packs consecutive messages into one record while they fit, so an
ordinary echo is one frame. A phone that attached below version 4 keeps one record per
message, which is what it was built to decode. No message changed: this is record framing,
not a new tag.

Measured on the device: 910.525 MHz / 500 kHz / SF5 / CR5, private channel, one echo is one
frame; the same exchange is two before version 4.

## Update, 2026-11-01 (profile version 6): a shell's folder in `Sessions`

The mesh picker drew every row as a name over a hex id (`ldeen` / `16db506fe9`), while the
tailnet launcher drew the directory the shell is in. The id is the `Attach` selector, not
something a person reads, and the folder is what tells two shells of the same kind apart.

| tag | direction | name | body |
|---|---|---|---|
| 0x85 | host to phone | `Sessions` | `flags u8` (bit 0 = last page, bit 1 = entries carry a `cwd`), then entries `id_len u8, id, label_len u8, label` and, when bit 1 is set, `cwd_len u8, cwd` |

- **`cwd`.** The daemon already describes the pane's live directory in `list` (`cwd` follows
  the keeper's `/proc` fact while a shell runs), so the bridge copies it into the entry instead
  of inventing a second source. A `cwd` is at most 60 bytes, cut at a character boundary, so
  one entry's worst case is `1 + 40 + 1 + 24 + 1 + 60 = 127` bytes: under `MAX_ROWS_BODY`'s
  130 and always inside one record.
- **Why a flag rather than a profile version.** A `List` is answered before most attaches
  succeed - the phone learns which shell to attach to from it - so the host cannot gate the
  page on `Profile.version`. The page carries its own format bit. The bridge remembers the
  last `Attach` version seen on that shell, refused or not, and sets bit 1 only for a phone
  that attached with `CWD_VERSION` (6) or later, so a version-5 phone still decodes the
  version-5 form.
- **Version 6.** `VERSION` is 6, `MIN_VERSION` stays 1, `CWD_VERSION` is 6. A version-5 phone
  is still accepted and answered in its own form; a phone that searches a list decodes whatever
  the flag says.
- **Phone rendering.** A row's second line is `tildify(cwd)` - the same function the tailnet
  launcher uses - and falls back to the id only when a version-5 host sends no path. The
  side panel's page title is the folder, from the same value.
- **Tests.** Protocol: `list_and_sessions_have_exact_bytes` (the version-5 and version-6
  forms side by side), `sessions_pages_fit_one_record_and_say_which_is_last` (both forms),
  `sessions_survive_any_chunking_and_cut_at_char_boundaries` (a 120-byte path), and the
  regenerated `mesh-term-vectors.json`, which the app's `MeshTermCodecTest` reads.
  Bridge: `a_list_carries_the_folder_only_to_a_phone_that_speaks_version_six`.
