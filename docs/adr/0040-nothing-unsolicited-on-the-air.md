# ADR-0040 — Nothing is transmitted that a person or a peer did not ask for

**Status:** Accepted (2026-11-13).

- **Date:** 2026-11-13
- **Related:** ADR-0032 and ADR-0035 (the route request, now answered but never originated),
  ADR-0037 (carrier selection), ADR-0039 (the batch transaction architecture),
  `docs/STATE.md` (the rule and its gates), `docs/RADIO-PLAN.md` §2 and §12.7b-2,
  `crates/session/src/endpoint.rs` (`stage_bootstrap`), `crates/session/src/radio.rs` (`pump`),
  `crates/daemon/src/mesh_carrier.rs` (`serve`, `harvest`)

## Context

The radio path is a guest on a public mesh. Every transmission is forwarded by every repeater in
range, and the people who run those repeaters pay for the airtime and the collisions.

On 2026-11-13 the operator's neighbours reported that the testing was breaking the mesh. They were
right, and the software had two loops that never stopped:

- the host asked for a route every **20 s** whenever it held none — a 44-byte **unscoped flood**,
  so every repeater in range forwarded it, 180 times an hour, for as long as the daemon ran;
- the phone's bootstrap fell back to a "slow rung" that restarted its whole fast ladder, so a
  nominal one-a-minute retry was **three records a minute**, for ever.

A first fix made both back off to a ten-minute ceiling. That is still unsolicited airtime, and it
still runs when nobody is using the radio: a phone left on the connect screen, or a host whose
phone is switched off, keeps transmitting indefinitely.

## Decision

**A transmission is caused by an operator action or by a peer's record, and by nothing else.**

- The host **never originates a route request**. It is reactive by construction: it answers a
  phone's request and never asks. The road it needs is the one the phone's record arrived by.
- The phone **never asks for a route on its own**. A path is learned from the peer's own records (a
  flooded arrival carries the chain home) and from the advert-cache read, which is a local command
  and costs no airtime.
- The bootstrap ladder is **spent and then goes quiet**. No slow rung, no doubling gap. The screen
  says "no answer from host · check the host is on the same channel, then refresh", and Refresh is
  the ask.
- Adverts stay off unless the operator sets `LDMUX_MESH_ADVERT_SECS`, which is the firmware's own
  companion default. **The advert on link open is gone too** (`RELEASE-AUDIT.md` 6.238): it was the
  last unsolicited transmission left, and it survived the first version of this decision because
  the reasoning behind it — that the radio's advert cache is the only place a path can come from —
  had already been disproved by `AR-10`.

The rule applies on **every** channel, public or private. A private tuple gets the same bounded
ladder and the same no-ask rule, so there is no second behaviour to test and nothing can flood a
shared band by accident.

## Consequences

- **A link that cannot come up costs three records, not a stream of them.** At range that means the
  operator presses Refresh; the alternative was a phone that floods a public mesh for ever.
- **Discovery is the bootstrap flood and nothing else.** A path is learned from the peer's records,
  so a link that has never carried a record has no direct path — which is correct, because there is
  nothing to send down it.
- **The route request is answer-only.** `build_request` has no production caller; the answer side
  stays live on both ends because a peer that asks must be answered (ADR-0032, ADR-0035).
- **The counters that prove it exist**: `an_idle_end_transmits_nothing` (an hour, no operator
  action, zero records) and `a_phone_that_cannot_reach_its_host_stops_flooding_the_mesh` (exactly
  the three rungs of the shipped ladder, then silence).

## Rejected

- **A backoff instead of a stop.** Tried first (`d5a3b09`): six floods an hour is better than a
  hundred and eighty and still unsolicited. The operator's requirement is "never", and a ceiling is
  not a stop.
- **A per-channel exception for private tuples.** Considered and refused: one rule is testable, and
  a private tuple is one configuration mistake away from a public one.
- **Keeping the host's ask as a pre-warm.** It buys a direct path a few seconds earlier and costs a
  flood every time the phone is away. The phone's own bootstrap teaches the same road.
