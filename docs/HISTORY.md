# How the radio path got here

**Written 2026-11-13 at `da0ed7d`.** A dated timeline with commit hashes, so a claim can be checked
with `git show`. The design is in `RADIO-PLAN.md`; the open work is in `RELEASE-AUDIT.md`; the
current state is `STATE.md`. This file is the *why it changed*, phase by phase.

## Phases

| Dates | Commits | Intent | What it produced |
|---|---|---|---|
| 2026-09-13 → 09-28 | `471a8da` … | a remote terminal over Tailscale, tmux control mode as the transport | the daemon, the CLI, the mobile client, the tailnet route |
| 2026-09-29 → 10-27 | `eb93c42` … | port the TypeScript tree to Rust, one crate per layer | `crates/{protocol,keeper,daemon,attach,vt-sys}`; the TS tree deleted |
| 2026-10-28 → 11-04 | `051f9f7`, `5e88d3b` | a LoRa route, first as a gateway beside the daemon | `crates/mesh` (the L2 client), a gateway process |
| 2026-11-05 → 11-06 | `d9b44c0`, `ca5ed7d` | the host owns its own radio; no Dart, no gateway | `crates/companion`, the daemon carrying the session itself |
| 2026-11-07 → 11-08 | `2cd643d`, `8c85dac`, `bafae10`, `cf0b421` | make the radio real: audit the companion protocol against the firmware tag, presets, the airtime law, a route | the first working link, in one room |
| 2026-11-09 → 11-10 | `838f101`, `a0cd1fa`, `0e9a122`, `11f8253` | the field failures: every wait scales with the route, a second carrier, a simulated 28-repeater mesh | flood carriage, route repair, timing derived from hops |
| 2026-11-11 | `2ebff42`, `9cf722e`, `23909aa`, `f5493ce` | stop patching: audit the whole path and rewrite it as a plan | `RADIO-PLAN.md`, ADR-0039, the five transformations `A1`–`A5` |
| 2026-11-12 | `0fe4cfd`, `cc5884b`, `383847b`, `24de2c0` | land the refactor: one engine, one ABI, one transport | v15 only, ~31,000 lines deleted, the B8 gate |
| 2026-11-13 | `7ac0650` … `da0ed7d` | field debugging at range, and the public-mesh rule | the route request restored, the boundary defects, the invariant |

## Reversals, and what each one cost

| What | From → to | Commit | Why |
|---|---|---|---|
| **How a route is found** | adverts only (ADR-0030) → **asked for** with a flooded `LDRQ` (ADR-0032) → **deleted** by the plan → **restored** | `75b4fe6` → `8c85dac` → `9cf722e` → `7ac0650` | a repeater drops an advert at 8 hops (`MyMesh.cpp:972`) and the mesh is 14–35. The plan deleted the request believing a sender learned its route from its own initiation; a flood's chain teaches the *receiver*, so it does not. `AR-17` |
| **Transport** | byte-stream ARQ + a 50 ms streaming bridge → message-oriented transactions | `23909aa` (ADR-0039), landed `cc5884b` | an ordered byte stream can only retransmit by number, so a record that no longer fitted was split mid-flight and delivered twice (`RC-7`) |
| **Input model** | live character-at-a-time → batch `Send`/`Refresh` | `0fe4cfd` | on a public channel every keystroke is a broadcast; the frame loop also burned 34 records/min idle |
| **Retry** | for ever, one a minute → doubling to 10 min → **never unsolicited** | `d5a3b09` → `da0ed7d` | the operator's neighbours reported the testing as breaking the mesh, and they were right |
| **Push classification** | `0x88` only → every push code → **one classifier for both ends** | `3ee2888` → `0020ec0` | the host had a second copy of the contract and refused every frame the phone accepted |
| **Session index** | stable per peer → per session | `cc00fbe` | a stable index removes the only signal that the peer restarted (`AR-19`) |
| **Acknowledgement** | a standalone ack per part → piggybacked, suppressed to the burst tail | `5940078` | a mid-burst ack collides at the first repeater (`AR-18`) |

## Deleted, and never coming back

- the 50 ms `Differ` loop, `Hold`, `Resync`, `owe_frame`, `Profile`/`Rows`/`Commit` (`0fe4cfd`, `cc5884b`);
- the byte-stream ARQ and `mesh_session.rs` (`cc5884b`);
- the Kotlin/Java protocol: `MeshcoreSession`, `MeshcoreFrameCodec`, `RadioRoute`, `AndroidRadioLink`, `vendor/libmeshcore` (`f7d0e1e`, `b4afff2`);
- the gateway process as the only way to reach a radio (`ca5ed7d`).

## Deleted, and brought back

- **the route request** (`8c85dac` → `9cf722e` → `7ac0650`). The only mechanism that finds a road
  past the eight-hop advert cap, and the one deletion that most directly explains "never worked at
  range".

## What the history teaches

1. **A plausible sentence in a plan becomes a design decision unless a test contradicts it.** The
   route request was deleted because one line said the initiation discovered the route. Nothing
   tested the claim, and the simulator shared the assumption.
2. **A refactor that deletes a capability must name its replacement and measure that the
   replacement reaches the same distance.** `AR-17` records four decisions of this shape.
3. **Two owners of one contract is a defect even when both are green.** The push classifier, the
   slot resolver and the list retry were all found on 2026-11-13, all in the seam between two
   components that each had tests.
4. **The gate measured the code; the requirement was the air.** Every field failure of 2026-11-12/13
   was an assumption the simulator shared, which is why the tree was green while the product never
   worked at range.
