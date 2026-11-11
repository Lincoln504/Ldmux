# Radio path audit ledger (append-only)

Started 2026-11-10. Purpose: identify **everything** that makes the radio path fail, stall or waste
airtime at range, with evidence, so an advanced model can act on it later. **No code changes are made
during this audit** — the only file written is this one.

## Severity

- `blocker` — the link cannot work, or works only by luck.
- `major` — works sometimes; wastes airtime or time badly; user sees a hang.
- `minor` — inefficiency or rough edge with no functional loss.
- `question` — needs a measurement or research before it can be graded.

## Entry schema

```
### A-<n> · <aspect> · <severity>
- Evidence: <file:line | commit | journal line | phone log line | doc | clone path:line>
- Finding: <what is actually true>
- Why it hurts at range: <causal chain to airtime, latency or a dead UI>
- Proposed fix (NOT applied): <one paragraph naming the file/constant it would touch>
- Open questions: <what a later round should check>
- Investigator: <id> · <time>
```

## Queue

| # | aspect | state |
|---|---|---|
| 1 | physical layer: preset, SF/BW/CR, sensitivity, airtime per record | pending |
| 2 | MeshCore forwarding: flood limits, scoping, rate limits, advert caps, dedup | pending |
| 3 | handshake: IK, sizes, timestamps, replay tolerance, pairing lifecycle | pending |
| 4 | carrier selection: direct/flood evidence rules, oscillation, cost | pending |
| 5 | session: ARQ, RTO vs hops, window, fragmentation, ordering | pending |
| 6 | attach flow: list/profile answer, one-record fit, waits, UI states | pending |
| 7 | batch mode: repaint size, compression, record ceiling | pending |
| 8 | live mode: keystroke latency, echo, cursor, resync | pending |
| 9 | paging, grid protocol, multi-shell | pending |
| 10 | phone lifecycle: instances, radio lease, mode vs transport, wakelocks | pending |
| 11 | host side: serial handling, scheduling, backpressure, restarts | pending |
| 12 | observability: what cannot be seen, refusal reasons, latency | pending |
| 13 | UX expectations at range: what the user is told and when | pending |
| 14 | security: replay, PSK, pinning, downgrade | pending |
| 15 | tests and simulation: untested paths, what mcsim could prove | pending |
| 16 | docs correctness: every claim against the code | pending |
| 17 | comparison with openhop_core / meshcore-pi behaviour | pending |
| 18 | alternatives: single-message terminal, compression-first, adaptive preset | pending |

## Ledger

