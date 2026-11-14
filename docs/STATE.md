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
- **A session at range** (above) is the only functional gap left in the radio path.
- **The app's own suite is not in the gate.** `scripts/android.sh test` is a separate command and
  CI builds Rust only, so two failures sat in it unnoticed until 2026-11-13 (both fixed in
  `8c772a0`). A suite nobody runs is a suite that is red.

## Decided 2026-11-13

- **Store-and-forward is not accepted as the product statement.** The arithmetic says interactive
  typing holds to ~12 hops; the operator's answer is that the target stays an interactive terminal
  at range, and the per-hop loss measurement is the gate rather than a reason to lower the bar.
- **One rule on every channel.** A private tuple gets the same no-unsolicited-transmission rule as
  a public one. Nothing floods a shared band by accident, and there is no second behaviour to test.
- **The background window is five minutes**, plus a hold while an operation is owed
  (`3bd3d17`). The 45 s window was right for the tailnet and wrong at range.
- **The desk window has scrollback** (`d3d8bdd`). It asks for the shell's history when the first
  snapshot arrives, because asking before the attach has a stream is refused.

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

**The per-hop loss on this mesh has never been measured.** Every ladder, timeout and retry in the
tree is sized from a guess. What exists is two anchors and one model:

- **The field's SNR range** (−6.75 to +5.5 dB against a −7.5 dB floor) implies anything from under
  1 % to over 50 % per hop. That is a range, not a number.
- **A community figure, claimed and not reproduced here**: per-hop resend (`max.resend 2`) moved
  delivery from 71 % to 95 % on a comparable topology (`RELEASE-AUDIT.md` R1). If 71 % is the
  end-to-end figure for that topology, the per-hop loss is whatever makes `(1−p)^n = 0.71`.
- **The model's own measurement**: a 12-hop flood at 20 % per-hop loss crosses about 7 % of the
  time (`crates/session/tests/endpoint.rs`), which is the arithmetic above and not the air.

The measurement is the first thing a range trip should take, and it is the one number every other
number in this file depends on.

**What the arithmetic forces.** Interactive typing is deliverable to roughly twelve hops at 10 %
loss or better. At thirty hops the honest product is **store-and-forward**: send a command, read the
result minutes later. That statement has not been accepted or rejected by the operator, and it
changes what the app is.

## Open questions

1. **What is the per-hop loss on the operator's mesh?** It gates every number above, and it is the
   first thing a range trip should take.
2. **What makes a bootstrap cross fourteen repeaters when a route request crosses twenty-two?** The
   counters exist (`sentRefused`, `ldmr`, `pushes`, `routeReplies`); the measurement does not. One
   mechanism is already known and already removed: on 2026-11-10 the old stack flooded
   `SELF_ADVERT`, three `LDRQ` and three `LDRH` within 20 ms of the handshake, so the first-hop
   repeaters were transmitting the precursor when `msg1` arrived and were deaf to it (half duplex;
   `RC-1`, `RADIO-PLAN.md` §5.1). Nothing is sent ahead of an initiation now, so that class is
   structurally gone, but it is the reason "the short one crosses and the long one does not" was
   ever true.
3. **Is live mode gone for good?** It was removed for the public channel, where a keystroke is a
   broadcast. On a private tuple the argument does not apply, and the decision is now "one rule
   everywhere", so it stays gone unless that is revisited.

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
