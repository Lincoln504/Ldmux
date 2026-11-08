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

## What a spent hour looks like

Measured on this box, 2026-11-07, after an afternoon of radio work on the public tuple:
`records  0 left this hour`, `airtime  57.6 s this hour`, `records 9 sent ... 14 pending`, and
`held back 20552 times by the airtime budget`. The allowance at 62.5 kHz is ≈66 records or 36 s.

Three things filled it, and only one of them was the operator's typing:

- **Live typing**, which the table above already prices at 36.5 s for one 40-character command. A
  single command line can be the whole hour.
- **Reconnects**: every handshake is three records each way, and their retransmissions are charged
  too. An afternoon of daemon and app restarts is a dozen handshakes.
- **Adverts**: one flood on open and one every five minutes, plus one when a peer's route hint
  arrives and no route exists yet.

The first two cannot be refused — a handshake is the peer asking to be heard, and refusing it makes
the host unreachable. The third can, and now is: **an advert is not sent once the hour's allowance
is spent** (`mesh_serial`). The budget therefore stops spending on broadcasts, and an hour can still
end over the arithmetic if a peer's handshakes and the acknowledgements for its traffic use it up.

**The hour is a rolling hour from the daemon's first send, and it is now written down.** The
allowance starts when the radio is opened and the spend is recorded in `<state>/mesh-airtime`, so a
restart resumes the same hour instead of handing out a fresh 36 s — measured 2026-11-07, an afternoon
of restarts reached `airtime 61.4 s` against a 36 s budget because every restart forgot what the last
one had spent. It is not a wall-clock hour and there is no official reset time in the firmware: the
companion protocol reports no airtime at all, which is why the meter is ours to keep.

**The two figures are ldmux's own arithmetic, not the radio's.** `airtime` is records sent multiplied
by one record's air at the current tuple, and `records left` is the allowance minus that count. The
firmware reports neither; `mesh status` used to label both as the radio's own, which is wrong and now
says what it is.

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
