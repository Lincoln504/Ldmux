# State of the project

**Written 2026-11-13, at the head of `fix/public-mesh-batch`.** One page: what works, what does
not, what is decided, and where the detail lives. Everything here is a file in the tree or a
command that was run; the long form is in `RELEASE-AUDIT.md` (open work, row by row) and
`RADIO-PLAN.md` (the radio design and its audit).

## What this is

A remote terminal: a daemon owns PTYs on a host, and a phone or a desk window attaches to them. Two
transports. **Tailnet** (WebSocket over Tailscale) works and is not in question. **Radio** (MeshCore
LoRa, a companion radio on each end) is the project's reason for existing and has never worked at
range.

## The story so far

The short version, because it is easy to lose in the row-by-row audit.

- **Before the refactor, the radio route worked with both radios in one room.** Batch mode, a shell,
  a command, its output. It never worked over public repeaters at range, and that was the whole
  reason for the audit.
- **The audit round (2026-11-11) rewrote the radio path.** The route request was deleted on a false
  premise, the byte-stream ARQ became message transactions, and the app moved onto the Rust engine.
- **After the refactor the operator has not seen a working shell on the phone.** The in-room case is
  proven by tests and by the host's journal — a version-15 session established at 1–2 hops on
  2026-11-13 and the phone's list request was acknowledged — but the screen never left "asking host
  for shells", because the daemon dropped the `List` before its own service saw it
  (`RELEASE-AUDIT.md` 6.235, fixed the same day). That is the regression the operator felt, and it
  was one line of slot resolution, not the transport.
- **At range it has never worked, before or after.** The bootstrap does not cross a long chain. The
  route request does cross one, which is why the two facts sit side by side in this file.

So the honest summary is: **in one room it worked, the refactor did not break it, a dropped `List`
did, and that is fixed but not yet seen working on the phone; at range it has never worked.**

## What works today

- **Tailnet route**: shells, attach, scrollback, ownership, marks, notifications, the desk window.
- **Radio route, in one room**: a version-15 session establishes, a shell lists, attaches, paints,
  takes a command and answers it. Proven by `crates/daemon/tests/mesh_full_stack.rs` over a
  simulated multi-repeater mesh, and on the air at 1–2 hops.
- **Route discovery crosses a real mesh**: the host answered `LDRQ` floods at 1, 2, 3, 4, 5, 6, 15,
  16, 19, 20, 21 and 22 hops in one session on 2026-11-13.
- **Direct carriage works over a learned chain**, and it is now asserted rather than assumed:
  `a_message_after_the_bootstrap_goes_direct_along_the_learned_chain` runs a bootstrap over four
  repeaters and requires the next message to go **direct along the chain** and arrive. The road
  comes from the **flooded copy** of the answer, not the direct one: a direct arrival has consumed
  its path, so it teaches nothing, and the flood copy is what carries the chain home. That is why
  the flood copy is not redundant.
- **The gate**: `scripts/test-parallel.py` (60 binaries, ~90 s) and
  `cargo clippy --workspace --all-targets -- -D warnings`, both green.

## What does not work

- **A session at range.** 2026-11-13, 14 repeaters: the phone sent 16 bootstrap rungs and the host
  heard none of them, while the same phone's route requests crossed 19–22 hops in the same hour.
  Whether the initiation is lost, sent down a stale direct path, or refused by the radio is not yet
  measured. The counters that answer it now exist (`sentRefused` on the phone, `ldmr` on the host).
- **The app's own suite is not in the gate.** `scripts/android.sh test` is a separate command and
  CI builds Rust only, so two failures sat in it unnoticed until 2026-11-13 (both fixed in
  `8c772a0`). A suite nobody runs is a suite that is red.

## Functional requirements, and where each one stands

The operator's requirements and the decisions taken, at the level they were stated, against the
implementation. A row that says MET names the test or the file; a row that says NOT MET names what
is missing. Nothing here is a number the operator did not give: where a figure appears it is a
measurement, a legal limit, or a model with its assumptions stated.

| Requirement | Status | Where |
|---|---|---|
| A terminal over LoRa **at range** | **NOT MET** — never worked at range | this file; `RADIO-PLAN.md` §12.7b-2 |
| Direct carriage normally, flood as the fallback | **MET in the model** (4 repeaters, asserted); unproven on the air | `a_message_after_the_bootstrap_goes_direct_along_the_learned_chain` |
| A route past the eight-hop advert cap | **MET** (answer side); the ask is no longer originated | ADR-0032, ADR-0035, ADR-0040 |
| Nothing unsolicited on the air | **MET** | ADR-0040; `an_idle_end_transmits_nothing` |
| Batch input: `Send`/`Refresh`, draft, paste, 3.5 s settle | **MET** | `RADIO-PLAN.md` §2; `crates/daemon/src/mesh_rpc.rs` |
| Five open shells, per-shell state | **MET** | `MAX_OPEN_PAGES`; `crates/session/tests/client.rs` |
| Scrollback on the desk window | **MET** | `the_desk_window_opens_on_the_shells_history` |
| Background: five minutes, plus a hold while owed | **MET** | `3bd3d17`; `BackgroundReleaseTest` |
| Ownership and take-back | **MET** | `Closed { HELD }`; `crates/daemon/tests/mesh_full_stack.rs` |
| Marks and notifications | **MET on the tailnet**; the radio replays them on `List`/`Attach` | `RADIO-PLAN.md` §2 |
| The airtime law, per band | **MET** | `docs/AIRTIME.md`; `crates/companion/tests/airtime_agreement.rs` |
| Path width follows the radio | **MET** | `RELEASE-AUDIT.md` 6.10, 6.14 |
| Store-and-forward at range | **REJECTED as a product statement** | this file, *Decided 2026-11-13* |
| Live character streaming on the radio | **REMOVED by decision** | ADR-0039; `0fe4cfd` |
| The per-hop loss measured | **NOT DONE** | this file, *Feasibility* |
| The app's own suite in the gate | **NOT DONE** | this file, *What does not work* |

## Decided 2026-11-13

- **Store-and-forward is not accepted as the product statement.** The arithmetic in *Feasibility*
  shows what a given per-hop loss costs at a given distance; the target stays an interactive
  terminal at range, and the per-hop loss measurement is the gate rather than a reason to lower the
  bar.
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
- Refresh is the ask;
- **no advert on link open**, on either end. An advert is a broadcast every repeater in range
  forwards, and the road it was meant to teach now comes from the bootstrap's own flood.

Gated by `an_idle_end_transmits_nothing` (an hour, no operator action, zero records) and
`a_phone_that_cannot_reach_its_host_stops_flooding_the_mesh` (three rungs, then silence).

**Why it exists.** Until 2026-11-13 the host asked for a route every 20 s and the phone's "slow
rung" restarted its whole ladder every 60 s, both for ever. The operator's neighbours reported the
testing as breaking the mesh, and they were right.

## Feasibility, stated plainly

The arithmetic is in `RADIO-PLAN.md` §12.7b-2. One transmission crosses `n` hops with probability
`(1−p)^n`; a screen update is a request and a response, so it costs `2/q` records; a full record is
602.6 ms at the shipped tuple. The two distances below are **illustrations, not a requirement** —
the operator has said only that the mesh can be longer than twenty hops and that repeaters cover
the whole city.

| per-hop loss | 12 hops (illustrative) | 30 hops (illustrative) |
|---|---|---|
| 5 % | ~5 attempts, one update in ~1 min | ~13 attempts, ~3 min |
| 10 % | ~9 attempts, ~2 min | ~70 attempts, ~11 min |
| 20 % | ~42 attempts, ~7 min | ~2500 attempts, hours |

**The per-hop loss on this mesh has never been measured.** Every ladder, timeout and retry in the
tree is sized from a guess. What exists is two anchors and one model:

- **The field's SNR range** (−6.75 to +5.5 dB against a −7.5 dB floor, from the 2026-11-09/10 logs)
  implies anything from under 1 % to over 50 % per hop. That is a range, not a number.
- **A community figure, claimed and not reproduced here**: per-hop resend (`max.resend 2`) moved
  delivery from 71 % to 95 % on a comparable topology (`RELEASE-AUDIT.md` R1). If 71 % is the
  end-to-end figure for that topology, the per-hop loss is whatever makes `(1−p)^n = 0.71`.
- **The model's own measurement**: a flood over twelve simulated repeaters at 20 % per-hop loss
  crosses about 7 % of the time (`crates/session/tests/endpoint.rs`), which is the arithmetic above
  and not the air.

The measurement is the first thing a range trip should take, and it is the one number every other
number in this file depends on.

**What the arithmetic says, and what was decided about it.** At the losses in the table, an
interactive update is minutes at the far end of a long chain, and the arithmetic's honest product
there is store-and-forward. **That is not accepted** (see *Decided 2026-11-13*): the target stays an
interactive terminal at range, so the per-hop loss measurement is the gate rather than a reason to
lower the bar. If the measurement comes back at 20 % per hop, the arithmetic and the target are in
direct conflict and that is a decision for the operator, not a footnote.

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

## Known issues, in one place

Everything open, at the level it matters, with where the detail is. Nothing here is a surprise to
`RELEASE-AUDIT.md`; this is the index to it.

**Blocking the product**

1. **A session at range has never worked.** The bootstrap does not cross a long chain. The counters
   that will say why exist (`sentRefused`, `ldmr`, `pushes`, `routeReplies`); the measurement does
   not. *This file, What does not work.*
2. **The per-hop loss is unmeasured**, so every ladder, timeout and retry is sized from a guess.
   *This file, Feasibility.*

**Open defects and limitations**

| Issue | State | Where |
|---|---|---|
| The host's USB companion can answer its own pushes and nothing else, and only a hard power cycle clears it | OPEN — the link loop re-opens with backoff, which is right; what is missing is a status line that says which commands went unanswered | `RELEASE-AUDIT.md` 6.11 |
| Upgrading loses the pairing: the package id changed for 0.1, so the previous install is a different app | OPEN by design | `RELEASE-AUDIT.md` 2.4 |
| A reviewer with no host, no tailnet and no radio cannot get past first run | OPEN by design — the listing says what it needs | `RELEASE-AUDIT.md` 2.5 |
| A pairing code is on screen while someone photographs it | OPEN — inherent; the token is bearer | `RELEASE-AUDIT.md` 4.4 |
| No release tag has ever been cut | OPEN | `RELEASE-AUDIT.md` 14.4 |
| Dictation depends on the device's speech recogniser, which may be cloud-based | accepted; the app says so | `RELEASE-AUDIT.md` Appendix C |
| RTL and split screen are unverified; dark mode is the only mode | accepted | `RELEASE-AUDIT.md` Appendix C |
| Nothing outside this machine builds the Android artifact | accepted | `RELEASE-AUDIT.md` 1.7 |
| The workspace-placement adapter (i3/sway/hyprland) is carried, not used | accepted — nothing asks for a workspace from a phone | `RELEASE-AUDIT.md` Appendix C |

**Process gaps**

- **The app's own suite is not in the gate.** `scripts/android.sh test` is a separate command and CI
  builds Rust only, so two failures sat in it unnoticed until 2026-11-13.
- **The gate measures the code, not the air.** Every field failure of 2026-11-12/13 was an
  assumption the simulator shared, which is why the tree was green while the product never worked at
  range (`RADIO-PLAN.md` §12.7b-3).
- **A contract with two owners has no cross-boundary test.** Three instances on 2026-11-13 alone
  (`AR-20`).

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
