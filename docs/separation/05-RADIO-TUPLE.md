# The radio tuple: five claims, one radio, and the options

- Written 2026-11-04. This document exists because the repository asserts five different radio configurations as the live one, and the choice between them has user-visible consequences that are not written down anywhere.
- **What the hardware is doing is decided, by date.** Two sources each say they read a preset back from a radio. `meshmore-sns` recorded 910.525 / 62.5 / SF7 / CR5 on 2026-10-28 (`meshmore-sns-radio-config.md:190` and its §11 session log). This repository recorded 914.25 / 500 / SF8 / CR5 / 22 dBm written and read back on **2026-11-02** (`docs/HARDWARE-VALIDATION.md:783`, `docs/MESH-STANDALONE.md:413-415`). The newer read-back wins for the deployed pair. Both name a node called `HELTEC-BASE`, and only one gives a public key (`meshmore-sns-radio-config.md:189`, no key at `HARDWARE-VALIDATION.md:781`), so either there are two units or one was re-tuned between the two dates, and neither reading changes the conclusion.
- **What is still open is the compiled default** (`Radio.kt`), the refusal rule, and which of the options below to take. Nothing here decides those.

## The five claims

| Claim | Where | What it says |
|---|---|---|
| The radio | `docs/MESH-STANDALONE.md:415`, `docs/HARDWARE-VALIDATION.md:783` | 914.25 MHz, 500 kHz, SF8, CR5, 22 dBm, written and read back on the device |
| The app default | `packages/mobile/composeApp/src/commonMain/kotlin/dev/ldmux/mobile/mesh/Radio.kt:66-73` | 910.525 MHz, 500 kHz, SF7, CR5, 22 dBm, one-byte path hash |
| The regulation argument | `docs/MESH-REGULATION-AND-CAPACITY.md:52-59` | 910.525 MHz at 500 kHz, because 500 kHz meets the bandwidth floor for the power argument. **Stale:** corrected to 914.25 MHz on 2026-11-04, and the argument survives the change |
| The gateway budget | `docs/MESH-GATEWAY.md:206,255,256` | 910.525 MHz, 62.5 kHz, SF7, CR5, 613 ms per 176-byte packet, 58.7 records per hour at 1 percent. **Superseded in place** by the correction at `:213-225`, which is 138 ms and 261 records per hour, so this row is history and not a live claim |
| The hardware notes | `docs/HARDWARE-VALIDATION.md:215,261,300,349`, `meshmore-sns-radio-config.md:53` | 910.525 MHz, 62.5 kHz, SF7, CR5 |

- The first two are the ones that matter, because one is what the hardware does and the other is what the app will do on a fresh install.
- They disagree on frequency and on spreading factor, so a fresh install cannot reach the host's radio.

## What must match for two nodes to hear each other

- Frequency, exactly.
- Bandwidth, exactly.
- Spreading factor, exactly.
- Coding rate, exactly.
- Path hash size, because the path encoding depends on it and the two ends must agree on how many bytes a hop occupies.
- Region and transport codes, if any are set, because they filter floods.

## The airtime arithmetic

- A 176-byte MeshCore packet at 62.5 kHz and SF7 is 613 ms (`meshmore-sns-radio-config.md:53`).
- The same packet at 500 kHz and SF7 is 75.3 ms, and at 500 kHz SF8 it is 137.9 ms, because one spreading factor step doubles the symbol time.
- All three come from the LoRa time-on-air formula with the 32-symbol preamble used below SF8. The same formula reproduces the 613 ms figure exactly, which is why the other two can be trusted without a fresh measurement.
- At a 1 percent duty cycle, which is 36 seconds per hour, that is about 58 records per hour at 62.5 kHz SF7, about 478 at 500 kHz SF7, and about 261 at 500 kHz SF8.
- Corrected 2026-11-04: an earlier pass in this directory used 66 and 132 ms, which gave 545 and 273 per hour. The ordering is unchanged and the absolute figures were high.
- Three other figures for the same 176-byte frame at 500 kHz SF7 circulate and they are **different models, not errors**: 75.3 ms here, 77 ms in `meshmore-sns-radio-config.md:61`, and 80 ms implied by `MESH-GATEWAY.md:269` (450 records an hour). They differ in preamble assumption, CRC inclusion, and a 176 or 172-byte payload. Quote the assumptions with the number. The 613 ms figure has independent corroboration (`meshmore-sns-radio-config.md:322-325`: "the lora airtime estimator, built from this preset, gives 612 ms ... the difference is rounding"), which is what makes the 75.3 and 137.9 derivations worth trusting.
- The United States has no duty cycle limit, so the 1 percent figure is a self-imposed policy, not a legal one. The firmware's own governor is a 50 percent budget by default (`_prefs.airtime_factor` 1.0, `src/Dispatcher.cpp:275`), which is far looser than LDMUX's meter.
- Consequence: every budget statement in `docs/MESH-GATEWAY.md` and `docs/MESH-LATENCY.md` is four to eight times more pessimistic than the link that is actually running.

## The argument the repository does not carry: what the centre frequency does to other people's meters

- MeshCore issue **#945** (opened 2025-10-15, still open, 414 comments, last activity 2026-11-03) is the on-record argument about wide channels in the US band, and it appears in no LDMUX document. The maintainers' refusal quotes a technical objection the research notes call the strongest on the record: running wider bandwidth that overlaps power-meter signals risks degrading the power company's meter network (`mesh-research/notes/topics/upstream-direction.md:51-57`), because utility meters in 902-928 MHz sit on about **300 kHz** channel spacing, so a **500 kHz** channel in the lower part of the band can straddle one or two meter channels.
- 914.25 MHz is in the lower part of the band. This is a coexistence question, not a compliance one, so it does not disturb the 15.247 reasoning in `MESH-REGULATION-AND-CAPACITY.md`. It is decision weight for the centre frequency and it belongs here rather than in a note nothing links to.
- The notes disagree on the issue's size, and the later one wins: `radio-legality-and-presets.md:141` says 377 comments and last active 2026-09-11, `upstream-direction.md` §2 says 414 and 2026-11-03.

## The options

### Option A: make 910.525 / 500 / SF7 / CR5 the tuple

- What changes: the radios are reflashed to the app's compiled default. The code stays as it is.
- What the user sees: a fresh install works against the host with no configuration step.
- What it costs: a reflash of every radio, and the regulation document's numbers become true.
- What it breaks: nothing in the code, but the hardware notes and the gateway budget must be rewritten, and the 62.5 kHz public MeshCore preset is still unreachable because the bandwidth differs.
- Risk: the reflash is the step that has already gone wrong once, and a radio left at 914.25 is silently unreachable.

### Option B: make 914.25 / 500 / SF8 / CR5 the tuple

- What changes: `RadioPreset.LDMUX_PRIVATE` becomes 914.25, SF8. The radios are untouched.
- What the user sees: a fresh install works against the host with no configuration step, and the link is about twice as slow as SF7.
- What it costs: the regulation document must be rewritten to 914.25, which is still inside 902 to 928 MHz and still 500 kHz wide, so the argument survives with different numbers.
- What it breaks: the app's default no longer matches the public MeshCore preset in any field, which is already true today.
- Risk: SF8 at 500 kHz is a non-standard combination that no other MeshCore client uses, so it is a private mesh by construction.

### Option C: no compiled default, adopt what the radio reports

- What changes: the constant disappears. On first run the app reads `SelfInfo`, shows the tuple, and adopts it. The host does the same. A send is refused, with a named reason, while the two ends disagree.
- What the user sees: one extra screen on first run, and a clear refusal instead of a silent no-route when the ends differ.
- What it costs: a UI surface for the tuple, a comparison on both ends, and a refusal state that has to be explained well enough not to look like a bug.
- What it breaks: nothing. It removes the class of defect rather than picking a winner.
- Risk: the refusal must not fire on a legitimate mismatch such as a deliberate reconfiguration, so it needs an explicit override.

### Option D: adopt the public MeshCore preset, 910.525 / 62.5 / SF7 / CR5

- What changes: the app default and the radios both move to 62.5 kHz.
- What the user sees: LDMUX can hear and be heard by public MeshCore nodes, which is the only option that gives any interoperability.
- What it costs: nine times the airtime per record, so the screen budget and the scrollback policy have to be rebuilt around 58 records per hour.
- What it breaks: the 500 kHz regulatory argument in `MESH-REGULATION-AND-CAPACITY.md:52`, which exists to justify the power level at a wider bandwidth.
- Risk: a private terminal link sharing a public mesh means every record is heard by every node in range, and the 36 s per hour meter becomes the binding constraint rather than a policy.

## The recommendation, stated as a recommendation

- Take Option C, because the radio is the only authority on what it is doing, and because a compiled default that disagrees with the hardware is the defect that produced this document.
- Then choose the physical tuple separately, as a deployment decision rather than a code constant. Option A if the radios can be reflashed reliably, Option B if they cannot.
- Do not take Option D unless public interoperability is a goal, because it costs nine times the airtime and abandons the bandwidth argument.

## What has to be true whichever option is chosen

- The path hash size must be part of the tuple, not a separate setting, because the raw-send encoding depends on it.
- The host must report its tuple and the phone must compare, because the host's radio is configured by a different process.
- The documentation must state one tuple per document, with the date it was observed, and never present a measurement as a configuration.
