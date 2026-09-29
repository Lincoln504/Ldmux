# Rust port log

The per-phase state/progress/findings record for the port (AGENTS.md PORT
MODE). The user compacts between phases; a fresh session resumes by reading
this file, then the plan §7 table, then the phase's source of record.

**Phase table** (plan §7, serial — no forks/sub-agents, ever):

| # | Status | Gate command | Gate state |
|---|---|---|---|
| 1 protocol | **done** (2026-09-28) | `cargo test -p ldmux-protocol` | **40 passed, 0 failed** (clippy: 8 non-blocking warnings, see Findings) |
| 2 pure modules | not started | per-module tests | — |
| 3 keeper ∥ vt-sys ∥ attach skeletons | not started | per §7 | — |
| 4 emulator + parity | not started | parity report: zero divergences | — |
| 5 daemon | not started | full TS-equivalent suite vs keeper | — |
| 6 desktop/restart/backpressure e2e | not started | §6 risk tests green | — |
| 7 build/deploy + D4 | not started | deploy green | — |

## Findings log (durable, dated)

- **2026-09-28, port opened.** Plan committed as operative (16cd5bf);
  ADR-0015 decision superseded; PORT MODE written into the project AGENTS.md
  (local-only, no forks/sub-agents, no live-system touching, serial phases,
  this log as the compact-resume record).
- **2026-09-28, phase 1 opened and closed.** Branch `port/rust` off
  16cd5bf. The `crates/protocol` crate is built and green (40 tests). Toolchain
  note: the flake's rustup is not yet wired (plan §5, a phase-7 build-system
  item); the gate currently runs on the **system** rustup (1.95.0, on PATH
  and reachable inside `nix develop`). The flake `runtime = [ rustup ]` change
  is pending — until then the gate command is `cargo test -p ldmux-protocol`
  (system toolchain). Record it here so a fresh session does not assume the
  flake already provides cargo.

## Phase 1 — protocol (DONE 2026-09-28)

**Scope (plan §4/§7):** the `protocol` crate — frames, messages (incl. the new
keeper messages), rpc, pairing, net, paths, socket — gate `cargo test -p
ldmux-protocol`.

**Completed (all in `crates/protocol`):**
- `frames` — the 8-byte frame codec, byte-exact (type u8, flags u8, reserved
  u16 BE=0, length u32 BE), encode/decode/`decode_json`, oversized-length
  refusal, partial-tail, subslice, property round-trip. Port of
  `packages/protocol/src/frames.ts`.
- `messages` — the full client control union, a serde `#[serde(tag="type",
  rename_all="lowercase")]` enum with `rename_all="camelCase"` per message,
  field-for-field from `messages.ts` (316 lines). The one modelling choice:
  `DesktopRequest`/`DesktopMessage` both use tag `desktop`, so they are one
  `Desktop { inventory: Option<...> }` variant (absent on a request, present on
  the answer) — emitted JSON is identical both ways. Verified by test
  (`desktop_request_has_no_inventory_result_has_one`,
  `snapshot_full_wire_shape_is_camel_case`).
- `keeper` — the new daemon↔keeper messages (plan §3.5): `Spawn, Kill, Write,
  Resize, Read, Snapshot{history_lines}, State, List, ChildInfo{pid},
  ChildExited, Close`. **No TS source of record** — the names are the plan's,
  the field sets are a **provisional spec** (finalised by the keeper phase,
  plan §7 phase 3). `Write.data` is a `String` (UTF-8, as the client's
  `Input.data` is), not raw bytes, so the message stays valid JSON.
- `rpc` — the shared data types (`DeviceInfo`, `SessionOwner`, `SessionRecord`,
  `SessionInfo`, `DesktopInventory` + workspace/window/pi-session, `PaneState`,
  `PingResult`, `NewParams`, `SendParams`), the method-name const set, and
  `DAEMON_PROTOCOL_VERSION`.
- `pairing` — `pairing_link`/`parse_pairing_link`/`split_host`, a byte-faithful
  `encodeURIComponent` + `URLSearchParams`-style decode (no `url` dep). Reads
  the **shared** TS fixture
  `packages/protocol/test/fixtures/pairing-links.json` (all three sides read it).
- `net` — `classify_address`/`is_tailnet_or_loopback` with a local IPv4/IPv6
  parser (no `node:net` BlockList); the TS test's classification table, ported
  1:1.
- `paths` — `state_dir`/`socket`/`tmux_socket` (the `tmux_socket` is marked
  **transitional** — the tmux server leaves with the port, §8/§9).
- `socket` — the newline-JSON Unix-socket `call` client (tokio), port of
  `socket-client.ts`: one call == one connection == one request/response line,
  timeout, error/refused/closed handling.

**Pending inside the phase:** none — phase complete. (See Open decisions.)

**Gate:** `cargo test -p ldmux-protocol` → **40 passed, 0 failed** (system
rustup 1.95.0, inside `nix develop`). TS gate re-run green: `node --run test`
347/347, `node --run typecheck` clean (the TS tree is untouched by this
phase).

**Open decisions / notes (for the next session):**
- **Typed client:** the Rust `socket::call` (generic over params/result) **is** the
  typed client; the TS's per-method `daemonCall` table is not mirrored as
  per-method wrapper functions (that surface is a CLI/daemon concern, phase 5).
  The method names live in `rpc::method`. Flag if a phase-5 consumer wants the
  per-method wrappers — they are a thin addition.
- **Clippy (non-blocking, per AGENTS):** 8 warnings — 3× manual ascii-range check
  (`pairing::is_unreserved`), 1× manual `!RangeInclusive::contains`
  (`pairing`), 4× collapsible `if`-in-`match` (`pairing` decode). Cosmetic; fix in
  a later pass or leave.
- **Edition 2024** requires rust ≥1.85; the system toolchain is 1.95.0, the
  flake's rustup (when wired) will be ≥ that. No action until the flake change.
