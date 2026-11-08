# What a session costs on the air

**Batched mode spends a small fraction of the air live mode spends for the same work.** Typing is
the whole of the difference — the answer is identical in both modes — and typing is where one record
per keystroke becomes one record per command. The measurements below come from
`crates/daemon/tests/airtime.rs`, which drives a real pane through the shipped bridge (the same
differ, the same record packing) with a link that counts every record and prices it with
`time_on_air_ms`; the citation for every number on this page is that test. Run it with
`cargo test -p ldmux-daemon --test airtime -- --ignored --nocapture`.

Three facts are worth stating plainly, because they are the reason the mode exists:

- **Typing** a command line costs ≈1/35 of live mode's air. The saving is almost entirely the echo:
  one record per keystroke each way becomes one record per command.
- **A whole command** — typing plus the answer — costs about 1/14 of live mode's air, because the
  answer does not get cheaper in either mode.
- **An agent turn** of several typed commands costs about a ninth, which is the figure that decides
  whether the public band is usable at all for agent work.

## The allowance, and why it is not one number

**A record's air is the tuple's, and the allowance is the band's.** At the US/Canada public preset
(910.525 MHz, 62.5 kHz, SF7, CR5) a full 172-byte record is **602.6 ms** on air. At the bench tuple
(500 kHz) it is 75.3 ms, and at 250 kHz/SF11 3.66 s. The firmware's own formula is the one used
(32 preamble symbols at SF8 and below, 20 header symbols, CRC, low-data-rate optimisation from
16 ms symbols), so a different preset is a different budget and nothing here is a constant.

- **869.4-869.65 MHz: one percent, 36 s an hour.** A 10 % duty cycle is a condition of use in that
  sub-band, and ldmux holds itself to a tenth of it. At 62.5 kHz that is **59 records** an hour.
- **Everywhere else: ten percent, 360 s an hour.** 902-928 MHz, where the MeshCore US/Canada preset
  sits, has no duty cycle for this kind of transmitter; the firmware itself permits a 50 % duty
  cycle (`getAirtimeBudgetFactor()` returns 1.0) and the Meshtastic ecosystem caps a node at 25 %.
  A tenth is a fifth of the firmware's ceiling and under half that cap. At 62.5 kHz it is **597
  records**.

The wire carries the countdown in one byte, so a host reports at most 255 whatever the policy
allows; the spend itself is counted exactly.

## What a spent hour looks like

Measured on this box, 2026-11-07, after an afternoon of radio work at 62.5 kHz under the old
one-percent allowance: `records  0 left this hour`, `airtime  57.6 s this hour`, `records 9 sent …
14 pending`, and `held back 20552 times by the airtime budget`. Three things filled it, and only one
of them was the operator's typing:

- **Live typing**, which the table below prices at 21.6 s for one 40-character command line with
  acknowledgements counted. One command could be very nearly the whole 1 % hour.
- **Reconnects**: every handshake is a record each way plus its acknowledgements, and an afternoon
  of daemon and app restarts is a dozen handshakes.
- **Adverts**: one flood on open and one every five minutes. That cadence was ldmux's own, 564 times
  the community's — a repeater is told to advertise every 47 hours — and it bought nothing, because
  a repeater drops an advert after eight hops anyway (ADR-0032). The default is now one hour.

**The hour is a rolling hour from the daemon's first send, and it is written down.** The allowance
starts when the radio is opened and the spend is recorded in `<state>/mesh-airtime`, so a restart
resumes the same hour instead of handing out a fresh one — measured 2026-11-07, an afternoon of
restarts reached `airtime 61.4 s` against a 36 s budget because every restart forgot what the last
one had spent. It is not a wall-clock hour and there is no official reset time in the firmware: the
companion protocol reports no airtime at all, which is why the meter is ours to keep.

**The two figures are ldmux's own arithmetic, not the radio's.** `airtime` is records sent multiplied
by one record's air at the current tuple, and `records left` is the allowance minus that count. The
firmware reports neither; `mesh status` says whose number each one is.

## Why this is measured rather than reasoned about

The radio's budget is counted in **records**, and a record costs its payload *plus* a packet header
and a path byte, so a record is never free however little it carries. Live mode sends one record per
keystroke and the host answers each keystroke with an echo; batched mode sends the whole line in one
record. That difference is invisible in a byte count of the session and obvious in a count of
records, which is why the harness counts both.

## Results (us-public tuple: 910.525 MHz, 62.5 kHz, SF7, CR5)

Numbers are milliseconds of air per direction, with acknowledgements counted in both directions.
Acks may ride on outgoing data (`ack_delay_ms`), so the data-only figure is the optimistic end; a
conclusion that holds at both ends is a conclusion about the modes rather than about the model.

| what | live mode | batched mode |
|---|---|---|
| typing a 40-character command | 21.6 s | 1.4 s |
| the answer to the command (30 lines) | 2.4 s | 2.4 s (the same work) |
| **a whole command, typing plus answer** | **52.3 s** | **3.9 s** |
| an agent turn: 8 typed commands, each answered | 234.2 s | 24.9 s |

- **Typing costs 97.2 % less air batched** (35x cheaper), and the saving is almost entirely the
  echo: one record per keystroke each way becomes one record per command.
- **A whole command is 7.4 % of live mode's air batched** — the answer is identical work in both
  modes, so the difference is everything the typing did not have to repeat.
- **An agent turn is 89.4 % less air batched.** A 1 % duty cycle (36 s an hour) buys 0.15 typed
  turns or 1.44 batched ones at this tuple; the 10 % allowance outside the European sub-band buys
  1.5 typed or 14.4 batched.

At the bench tuple (500 kHz, SF7, CR5) every record is eight times cheaper in air, so the same turn
is ≈16 s typed and 2.0 s batched. The ratio between the modes belongs to the modes; the absolute
numbers belong to the tuple.

## What follows

- Batch mode is not a preference; on a public channel it is the only way an agent turn fits any
  sensible allowance. That is why a public tuple forces it (`Config::meshHoldsInput`).
- `rto_initial_ms` scales with the airtime (`Config::with_airtime`): on this tuple a record is about
  600 ms, so a fixed two-second retry would duplicate work the peer already has, and duplicates are
  airtime.
- **Discovery is a request, not a broadcast** (ADR-0032). A route request is ≈40 bytes and its
  direct answer ≈40 bytes plus the path, spent once per attempt instead of once per interval: at
  62.5 kHz that is ≈0.5 s of air for both, against 449 ms for one 120-byte advert that a repeater
  will not carry past eight hops.
- **The advert is a maintenance broadcast** at `LDMUX_MESH_ADVERT_SECS`, default one hour, ≈0.07 % of
  a ten-percent allowance per advert at this tuple.

## Method notes, so the numbers can be argued with

- The pane is a real shell (`common::rig::Rig`), the bridge is the shipped one, and the link records
  exactly what the bridge hands it: no simulation of the differ or of the packing.
- A record's air time uses the payload average per record, which understates a session of mixed
  sizes (air time is convex in length); the mode comparison is unaffected because both sides are
  measured the same way.
- Acknowledgements are counted as one record (12 bytes) per data record, in the opposite direction.
  Real piggybacking makes both modes cheaper and the live one more so, never the other way round.
- The 20 ms tick between keystrokes is faster than a person types and slower than the radio; it does
  not change the record count, only how the echo is grouped.
