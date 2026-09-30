# Rust port log

The per-phase state/progress/findings record for the port (AGENTS.md PORT
MODE). The user compacts between phases; a fresh session resumes by reading
this file, then the plan §7 table, then the phase's source of record.

**Phase table** (plan §7, serial — no forks/sub-agents, ever):

| # | Status | Gate command | Gate state |
|---|---|---|---|
| 1 protocol | **done** (2026-09-28) | `cargo test -p ldmux-protocol` | **40 passed, 0 failed** (clippy: 8 non-blocking warnings, see Findings) |
| 2 pure modules | **done** (2026-09-28) | `cargo test -p ldmux-daemon` | **88 passed, 0 failed** |
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

## Phase 2 — pure modules (DONE 2026-09-28)

**Scope (plan §7):** the pure, dependency-free daemon modules, each a 1:1
port of its named `packages/daemon/src/…` file with its TS test ported 1:1.
Crate: `crates/daemon` (package `ldmux-daemon`), depends on `ldmux-protocol`.
Gate `cargo test -p ldmux-daemon` → **88 passed, 0 failed** (system rustup
1.95.0, inside `nix develop`). Clippy on the daemon crate: **0 warnings**.

**Modules (all in `crates/daemon/src/`):**
- `auth` (10 tests) — `load_or_create_token`/`token_matches`; 0600 token file,
  constant-time compare, the legacy-path move/adopt rule. Port of `auth.ts`.
- `qr` (5 tests) — full ISO/IEC 18004 encoder (byte mode, v1–40, L/M/Q/H):
  GF(256)/Reed–Solomon, block interleave, placement, 8-mask penalty, plus
  `render_qr_terminal` (xterm-256 half-block, explicit dark/light) and
  `render_qr_pbm`. Port of `qr.ts`. **The tables are the load-bearing part** —
  see Findings. Correctness is pinned by decoding every size back with
  `zbarimg` (the 160-code sweep), not by comparing bits to a second encoder.
- `sanitize` (16 tests) — the stateful pane-byte sanitizer (OSC 52, DCS/APC,
  DA/DSR) FSM. Port of `sanitize.ts`.
- `screen_diff` (10 tests) — "what changed on screen" from two captures,
  `UpdateKind` enum. Port of `tmux/screen-diff.ts`.
- `ownership` (16 tests) — who is looking at a session (ADR-0010): claim /
  re-claim / takeover / release / retain, `seizing_closes_the_desk_window`.
  Port of `ownership.ts`.
- `registry` (10 tests) — the durable session index: typed `load` (drops a
  record that does not shape into `SessionRecord`), atomic `save`, two-pass
  `prune`, `reconcile`. Port of `registry.ts`.
- `cwd` (8 tests) — `resolve_cwd`/`home_dir`; `None` covers absent/null/
  non-string. Port of `cwd.ts`. `$HOME` read at call time; a clear panic when
  unset (divergence from the TS passwd fallback — untestable edge, logged).
- `exec` (7 tests) — `run`: async, both pipes drained concurrently (a full pipe
  deadlocks otherwise), bounded; SIGTERM at the deadline then SIGKILL after the
  grace (a SIGTERM alone is not a bound). Port of `exec.ts`.
- `proc` (0 tests — exercised through `exec`) — `spawn_captured` (tokio +
  `setsid` pre_exec, own process group), `kill_group` (negative-pid group kill
  with single-pid fallback), `wait_drained` (wait **while** draining both pipes
  concurrently — the ordering is the point). Port of `proc.ts`.
- `pairing` (6 tests) — `pairing_for`/`ListenInfo`/`PairingProblem` for the
  address this daemon listens on. Port of `pairing.ts` (the *daemon-side*
  pairing, distinct from the protocol-crate `pairing` done in phase 1).

**Pending inside the phase:** none — phase complete.

**Gate:** `cargo test -p ldmux-daemon` → **88 passed, 0 failed**. TS gate re-run green: `node --run test` 347/347,
`node --run typecheck` clean (the TS tree is untouched by this phase).

**Findings / decisions (this phase):**
- **The QR ECC tables were transcribed by hand and drifted** (a 45-element row
  where the source has 40). Regenerated **both** `ECC_CODEWORDS_PER_BLOCK` and
  `ECC_BLOCKS` programmatically from `packages/daemon/src/qr.ts` (the TS
  source-of-record), asserting each of the 8 rows is exactly 41 elements. The
  index-0 sentinel is `-1` in TS (unrepresentable in `u32`); it is `0` here and
  never read (indexing starts at version 1). This is the one place a silent
  transcription error would have shipped a mis-encoded QR that still "passed"
  its own render test — the `zbarimg` decode sweep is what catches it.
- **`with_ecc` block-length direction.** TS: short blocks get
  `shortLen - eccLen`, the *later* blocks get `+1` (`i < shortBlocks ? 0 : 1`).
  The first draft had the ternary backwards; a 1-line fix, but it is exactly the
  kind of error the per-version decode sweep exists to catch.
- **`draw_codewords` zig-zag underflow.** TS `right -= 2` is fine at
  `right = 1` (JS: `1 - 2 = -1`, loop ends). Rust `u32` underflowed. Guarded
  with `if right <= 2 { break; }`. Same class in `alignment`:
  `pos.len() - 1` for version 1 (no alignment patterns) → `saturating_sub`.
- **`run` must drain while it waits.** The child cannot exit until its pipes
  are drained, so `wait().await` *then* `drain()` deadlocks the moment a stream
  exceeds the kernel buffer (the 1 MiB test). `Spawned::wait_drained` drives
  the two pipe reads and the exit wait in one `tokio::join!`.
- **`proc` group-kill uses a negative pid** (`kill(-pid)`), the child being in
  its own session from `setsid`; the single-pid fallback covers a reaped pid.
  The `pre_exec` closure returns `Result<(), io::Error>` in tokio 1.53 (not an
  `i32`), so `setsid`'s `Result` is mapped, not returned.
- **Divergences (untestable edges, logged):** `cwd::home_dir` reads `$HOME`
  and panics with a clear message when unset (TS has a passwd fallback);
  `sanitize`'s overflow guard is byte-based (Rust) vs UTF-16-unit-based (TS) —
  stricter for multibyte; the pairing name trim counts Rust chars vs JS UTF-16
  code units (same count for the inputs that matter).
- **Clippy:** the daemon crate is **clean** (0 warnings). The `rand::rng()`
  rename (was the deprecated `thread_rng`) was taken; no new runtime deps were
  added beyond the allowlist (`serde`, `serde_json`, `tokio`, `uuid`, `nix`,
  `rand` — `rand`/`nix` are used only by `auth`/`proc`/`exec`).
