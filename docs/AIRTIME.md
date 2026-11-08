# What a session costs on the air

**Batched mode spends a small fraction of the air live mode spends for the same work.** Typing is
the whole of the difference — the answer is identical in both modes — and typing is where one record
per keystroke becomes one record per command. The measurements below come from
`crates/daemon/tests/airtime.rs`, which drives a real pane through the shipped bridge (the same
differ, the same record packing) with a link that counts every record and prices it with
`time_on_air_ms`; the citation for every number on this page is that test. Run it with
`cargo test -p ldmux-daemon --test airtime -- --ignored --nocapture`.

Three facts are worth stating plainly, because they are the reason the mode exists:

- **Typing** a command line costs ≈1/6 of live mode's air. The saving is almost entirely the
  echo: one record per keystroke each way becomes one record per command.
- **A whole command** — typing plus the answer — costs about a quarter of live mode's air, because the
  answer does not get cheaper in either mode.
- **An agent turn** of several typed commands costs about a seventh, which is the figure that decides
  whether the public band is usable at all for agent work.

## Why this is measured rather than reasoned about

The radio's budget is counted in **records** (`recordsLeftThisHour`), and a record costs its payload
*plus* a packet header and a path byte, so a record is never free however little it carries. Live
mode sends one record per keystroke and the host answers each keystroke with an echo; batched mode
sends the whole line in one record. That difference is invisible in a byte count of the session and
obvious in a count of records, which is why the harness counts both.

## Results (us-public tuple: 910.525 MHz, 62.5 kHz, SF7, CR5)

Numbers are milliseconds of air per direction, with acknowledgements counted in both directions.
Acks may ride on outgoing data (`ack_delay_ms`), so the data-only figure is the optimistic end; a
conclusion that holds at both ends is a conclusion about the modes rather than about the model.

| what | live mode | batched mode |
|---|---|---|
| typing a 40-character command | 36.5 s | 5.6 s |
| the answer to the command (30 lines) | 4.3 s | 4.3 s (the same work) |
| **a whole command, typing plus answer** | **40.8 s** | **9.9 s** |
| an agent turn: 8 typed commands, each answered | 158.9 s | 21.1 s |

- **Typing costs 84.6 % less air batched** (6.5x cheaper), and the saving is almost entirely the
  echo: one record per keystroke each way becomes one record per command.
- **A whole command is 24.4 % of live mode's air batched** — the answer is identical work in both
  modes, so the difference is everything the typing did not have to repeat.
- **An agent turn is 86.7 % less air batched.** A 1 % duty cycle (36 s an hour) buys 0.23 typed
  turns or 1.7 batched ones at this tuple.

At the bench tuple (500 kHz, SF7, CR5) every record is ≈4.4 times cheaper in air, so the same
turn is ≈36 s typed and 4.8 s batched. The ratio between the modes belongs to the modes; the
absolute numbers belong to the tuple.

## What follows

- Batch mode is not a preference; on a public channel it is the only way the allowance survives an
  agent turn. That is why a public tuple forces it (`Config::meshHoldsInput`).
- `rto_initial_ms` scales with the airtime (`Config::with_airtime`): on this tuple a record is about
  550 ms, so a fixed two-second retry would duplicate work the peer already has, and duplicates are
  airtime.
- The advert costs one record every 30 minutes, ≈0.05 % of the allowance, and it is what makes
  a repeater-reached phone reachable at all.

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
