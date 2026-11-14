# State of the project

**Written 2026-11-13, at `da0ed7d` on `fix/public-mesh-batch`.** One page: what works, what does
not, what is undecided, and where the detail lives. Everything here is a file in the tree or a
command that was run; the long form is in `RELEASE-AUDIT.md` (open work, row by row) and
`RADIO-PLAN.md` (the radio design and its audit).

## What this is

A remote terminal: a daemon owns PTYs on a host, and a phone or a desk window attaches to them. Two
transports. **Tailnet** (WebSocket over Tailscale) works and is not in question. **Radio** (MeshCore
LoRa, a companion radio on each end) is the project's reason for existing and has never worked at
range.

## What works today

- **Tailnet route**: shells, attach, scrollback, ownership, marks, notifications, the desk window.
- **Radio route, in one room**: a version-15 session establishes, a shell lists, attaches, paints,
  takes a command and answers it. Proven by `crates/daemon/tests/mesh_full_stack.rs` over a
  simulated multi-repeater mesh, and on the air at 1–2 hops.
- **Route discovery crosses a real mesh**: the host answered `LDRQ` floods at 1, 2, 3, 4, 5, 6, 15,
  16, 19, 20, 21 and 22 hops in one session on 2026-11-13.
- **The gate**: `scripts/test-parallel.py` (60 binaries, ~90 s) and
  `cargo clippy --workspace --all-targets -- -D warnings`, both green.

## What does not work

- **A session at range.** 2026-11-13, 14 repeaters: the phone sent 16 bootstrap rungs and the host
  heard none of them, while the same phone's route requests crossed 19–22 hops in the same hour.
  Whether the initiation is lost, sent down a stale direct path, or refused by the radio is not yet
  measured. The counters that answer it now exist (`sentRefused` on the phone, `ldmr` on the host).
- **The desk window has no scrollback.** The daemon can serve it (`scrollback` request,
  `SnapKind::History`) and the mobile tailnet client asks; `crates/attach` never does. Branch
  `fix/desktop-attach-history` is an unfinished attempt (its autowrap half landed separately).
- **The background rule is not the frozen one.** `RADIO-PLAN.md` §2 says a 5-minute idle grace plus
  a hold while an operation is owed; the code has one flat 45 s window that closes the radio engine
  (`RELEASE-AUDIT.md` 6.237). At range every return costs a full bootstrap.
- **The app's own suite is not in the gate.** `scripts/android.sh test` is a separate command and
  CI builds Rust only, so two failures sat in it unnoticed until 2026-11-13 (both fixed in
  `8c772a0`). A suite nobody runs is a suite that is red.

## The public-mesh rule

**Nothing is transmitted that a person or a peer did not ask for.** Both ends hold it as of
`da0ed7d`:

- the host never originates a route request; it answers and never asks;
- the phone never asks for a route on its own; a path comes from the peer's records and from the
  advert cache read, which is a local command;
- the bootstrap ladder is spent and then goes quiet, and the screen says
  "no answer from host · check the host is on the same channel, then refresh";
- Refresh is the ask.

Gated by `an_idle_end_transmits_nothing` (an hour, no operator action, zero records) and
`a_phone_that_cannot_reach_its_host_stops_flooding_the_mesh` (three rungs, then silence).

**Why it exists.** Until 2026-11-13 the host asked for a route every 20 s and the phone's "slow
rung" restarted its whole ladder every 60 s, both for ever. The operator's neighbours reported the
testing as breaking the mesh, and they were right.

## Feasibility, stated plainly

The arithmetic is in `RADIO-PLAN.md` §12.7b-2. One transmission crosses `n` hops with probability
`(1−p)^n`; a screen update is a request and a response, so it costs `2/q` records; a full record is
602.6 ms at the shipped tuple.

| per-hop loss | 12 hops | 30 hops |
|---|---|---|
| 5 % | ~5 attempts, one update in ~1 min | ~13 attempts, ~3 min |
| 10 % | ~9 attempts, ~2 min | ~70 attempts, ~11 min |
| 20 % | ~42 attempts, ~7 min | ~2500 attempts, hours |

**The per-hop loss has never been measured.** Every ladder, timeout and retry in the tree is sized
from a guess, and the field's SNR range (−6.75 to +5.5 dB against a −7.5 dB floor) implies anything
from under 1 % to over 50 %. That measurement is the first thing a range trip should take.

**What the arithmetic forces.** Interactive typing is deliverable to roughly twelve hops at 10 %
loss or better. At thirty hops the honest product is **store-and-forward**: send a command, read the
result minutes later. That statement has not been accepted or rejected by the operator, and it
changes what the app is.

## Open questions

1. **Is the product interactive or store-and-forward at range?** The arithmetic says the second
   beyond ~12 hops; the app is built for the first.
2. **What is the per-hop loss on the operator's mesh?** It gates every number above.
3. **Does a private mesh get different behaviour from a public one?** Today the distinction lives
   only in the UI (`meshPublicChannel` forces batch mode). The radio layer behaves identically, so a
   private tuple gets the same bounded ladder and the same no-route-ask rule. If a private mesh
   should retry persistently, or stream live, that is a decision and the code has a clean place for
   it.
4. **Live mode is gone by decision** (`RADIO-PLAN.md` §2, removed in `0fe4cfd`). Is that permanent
   for a private channel too, where a keystroke is not a broadcast?
5. **What is the background contract?** 45 s and the radio engine closes, or the frozen 5-minute
   grace? At range the difference is a full bootstrap on every return.
6. **Does the desk window need scrollback?** The daemon can serve it; no client asks.

## Where the detail lives

| Document | What it is |
|---|---|
| `docs/ARCHITECTURE.md` | the deployed design: layers, wire, invariants, security |
| `docs/RADIO-PLAN.md` | the radio redesign, the frozen product decisions, and the audit (`AR-1`..`AR-20`) |
| `docs/RADIO-AUDIT-REVIEW.md` | the root-cause, firmware and field-log audit behind the plan |
| `docs/RELEASE-AUDIT.md` | open work, row by row, with the evidence for each |
| `docs/HISTORY.md` | how the radio path got here: phases, reversals, and what each one cost |
| `docs/AIRTIME.md` | what an hour buys, and the law in each band |
| `docs/adr/` | the decision record, indexed in `docs/adr/README.md` |
