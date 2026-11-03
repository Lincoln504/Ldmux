# Regulation, capacity and channel occupancy, and what they mean for this link

Written 2026-11-03. The question behind this document is whether LoRa is "too slow for a
terminal" by nature, whether a duty cycle really applies to every frequency we might use, and
whether we can tell — honestly — whether a channel already has somebody on it.

Every number below names the document it comes from. Where a claim is about our own code it has a
`file:line`.

## 1. The duty cycle is a property of a sub-band, not of "LoRa"

The folklore value, "1 % on 868 MHz", is wrong in three ways at once.

- **The value varies by sub-band.** ETSI EN 300 220-2 Table 4 (V3.3.1, 2025-01) gives 863–865 MHz
  at 0.1 %, 865–868 MHz at 1 %, 868.0–868.6 MHz at 1 %, 868.7–869.2 MHz at 0.1 %,
  **869.4–869.65 MHz at 10 %**, and 869.7–870 MHz at either 5 mW with no requirement or 25 mW at
  1 %.
- **Three slots are excluded outright.** 868.6–868.7, 869.2–869.4 and 869.65–869.7 MHz are alarm
  bands, regulated by EN 300 220-3-1/3-2 and prohibited to general SRDs under band V and W.
- **The accounting unit is the sub-band.** EN 300 220-1 clause 3.1 defines the duty cycle over an
  observation bandwidth `Fobs` that defaults to "the Permitted Frequency Band", and EN 300 220-2
  clause 4.4.3.2 repeats it. So a transmitter in 868.0–868.6 MHz has 1 % of 600 kHz, i.e. 36 s of
  cumulative on-time per hour; one in 869.4–869.65 MHz has 10 % of 250 kHz, i.e. 360 s. The two
  are separate entitlements, not alternative readings of one rule, and the standard says the limit
  of each band applies to the transmissions within that band.

**Hopping does not multiply the budget.** ERC/REC 70-03 note 1 says the duty cycle "applies to the
entire transmission (not to each hop channel)", and the LoRaWAN Regional Parameters state that the
specification "exclusively uses duty-cycled limited transmissions" — hopping belongs to the other
access regime (LBT/AFA), not to a per-channel allowance. Within one device the budget is
device-wide per sub-band; across devices each node carries its own, which is why twenty nodes on
one channel hold twenty separate 36 s/hour entitlements.

**Polite spectrum access is usually the worse bargain.** It replaces the duty cycle with a CCA plus
a cumulative cap of 100 s per hour per 200 kHz (Table 18), a 1 s maximum transmission and a 100 ms
minimum off-time. For a 125 kHz signal in the 250 kHz band O that converts 360 s/hour into
100 s/hour — a 5.6 dB loss of airtime — and adds a mandatory listen before every record.

## 2. The United States has no duty cycle at all, and that changes what our presets mean

`47 CFR 15.247` contains no duty-cycle limit. What it has is a per-channel occupancy cap:

- FHSS: for a 20 dB bandwidth below 250 kHz, at least 50 hopping frequencies and an average
  occupancy on any frequency no more than 0.4 s within 20 s; 250 kHz or wider, at least 25
  frequencies and 0.4 s within 10 s; maximum 20 dB bandwidth 500 kHz.
- Digital modulation: minimum 6 dB bandwidth 500 kHz, 1 W conducted, and a power spectral density
  no more than 8 dBm in any 3 kHz "during any time interval of continuous transmission".

Two things follow for this project.

- **Our current preset is on the digital-modulation path, not the FHSS path.** The live radio runs
  910.525 MHz at **500 kHz** bandwidth, which meets the 500 kHz floor, and at that width the 8 dBm
  per 3 kHz density limit and the 1 W ceiling coincide. There is therefore no duty-cycle and no
  dwell constraint on it. The earlier arithmetic in `docs/MESH-LATENCY.md` that assumed a
  European 1 % is the *worst* case, not ours.
- **A narrow fixed channel would not be authorised there.** A single 125 kHz channel neither hops
  nor meets the 500 kHz floor, so 15.247 offers no route for it; 15.249 does, but at roughly
  −1.25 dBm EIRP, some 25 dB below what we run. The 500 kHz preset is not an accident of tuning,
  it is what makes the link legal in the US.

Elsewhere the picture inverts again: Australia's LIPD class licence 2025 attaches **no duty cycle,
no dwell and no bandwidth cap** to 915–928 MHz (1 W EIRP, with a 20-hop minimum for the hopping
class and a 25 mW/3 kHz density limit for the digital class); New Zealand's SRD GURL has no
time-based limit in that band either; India is 1 % at 865–868 with no LBT alternative; Japan's
10 % applies only to 922.4–923.4 MHz, with 360 s/hour; Korea uses LBT rather than a duty cycle for
LoRaWAN.

**So the architecture does not need to change**, and the reason is stronger than "nobody is
watching": on the band we actually use, in the country we actually use it, the regulator imposes no
airtime fraction. The European 1 % remains a design constraint worth respecting because a user in
Europe would be on 869.4–869.65 MHz at 10 % or 868.0–868.6 MHz at 1 %, and the software should
behave sensibly there without being told which continent it is on.

## 3. What MeshCore itself does, read from the source

- **Its duty-cycle limiter is a device-wide token bucket whose ratio is an operator scalar.**
  `Dispatcher::begin()` computes `duty_cycle = 1/(1 + airtime_factor)` over a one-hour window and
  refills continuously; `getAirtimeBudgetFactor()` returns 1.0, so the **default is 50 %**. There is
  no region table and no knowledge of frequency: `CommonRadioPrefs` exposes only scalars
  (`set freq 150.0–2500.0`, `set rf`, `set bw`, `set sf`, `set af 0–9`).
- **A token bucket also allows a burst.** It starts full — a node configured for 10 % has six
  minutes of airtime available immediately — and bounds the hourly average, not the instantaneous
  rate. That is defensible engineering and it is not the same thing as a regulator's 1 %.
- **Channel sensing is off by default, and the transmit gate does not use the good sensor.**
  `checkSend()` consults `isReceiving()` only; the RSSI-plus-CAD path lives in
  `RadioLibWrapper::isChannelActive()` and is not called from that gate in the retrieved source.
  `getCADEnabled()` returns false and `getInterferenceThreshold()` returns 0, and the repeater
  constructor sets both to off.
- **The operator-facing claim is narrower than the code.** The v1.14.1 announcement presents the
  token bucket as staying "within regional transmission limits without manual calculation"; what the
  code enforces is whatever factor the operator typed, defaulting to 50 %.

None of this is an argument against MeshCore as a carrier. It is the reason we run our own
session, our own pacing, and our own airtime budget on top of it.

## 4. Whether a channel is already in use — what can and cannot be known

Encryption is irrelevant to this question. Everything a detector examines — preamble, the explicit
header with its own CRC, the payload CRC — sits outside the encrypted payload, so a foreign LoRa
network on our parameters is observable whether or not its contents are readable.

**RSSI is a veto, never a clearance.** Semtech says it in both the SX126x datasheet and AN1200.85:
because a LoRa receiver demodulates below its noise floor, "the use of the RSSI in this situation
would clearly be impracticable". Absence of energy above a threshold therefore does not exclude a
decodable signal. RSSI also carries no modulation information: a jammer, a BLE burst and a Wi-Fi
sidelobe raise it equally, and the SX126x's +10 dBm absolute maximum input means a nearby
transmitter can saturate the front end into a meaningless reading.

**CAD is the designed sensor, and it is good but not a guarantee.** On the SX126x, CAD correlates
against LoRa chirps — preamble *and* data symbols, unlike the SX127x which detects preamble only —
for 1/2/4/8/16 symbols, and reports `CadDone` plus `CadDetected`. Semtech's own measurements
(AN1200.48) give, at the sensitivity limit, 98.6 % detection with 0.17 % false positives at SF7 /
125 kHz, 99.4 % with 0.09 % false at SF11, and 97.5–99.8 % across SF9/SF10. It is tuned by
`cadDetPeak`, which trades sensitivity for false alarms — at SF11 with 4 symbols, `cadDetPeak` 20
reports detection 95 % of the time *with the generator switched off*. CAD costs time: 1.6 ms at
SF7/125 kHz for one symbol, 60 ms at SF12 for one, 552 ms at SF12 for sixteen.

**Field measurements are less encouraging than the bench.** Pham and Ehsan (Sensors 21(3):825,
2021) found SX1262 CAD stable only to 1.3 km and "no significant difference" from the SX1276, with
**no false positives at all** — CAD is asymmetric: a positive is trustworthy, a negative is not. A
hidden node is invisible by construction, and the capture effect lets a stronger transmitter win
the demodulator even when both are present.

**What that means for us, concretely.** A useful "is this channel free?" answer is a measurement,
not a verdict, and it needs all four of:

1. **Noise floor** from the radio's own calibration (SX126x, averaged over ~64 samples in RX).
2. **CAD hit rate** at our exact SF/BW over a dwell long enough to cover a few frames — one second
   at SF7/500 kHz is many symbol periods, and CAD is cheap there.
3. **Frames heard but not ours.** The radio's own statistics counters — `GetStats` on the SX126x
   gives `NbPktReceived`, `NbPktCrcError` and `NbPktHeaderErr` — count foreign traffic including
   packets we cannot decrypt. A rising `NbPktReceived` with no frames from our own peers is direct
   evidence of another network.
4. **Our own traffic on the same parameters**, which the bridge already knows.

The honest output is a band, not a boolean: "quiet" (no energy above the floor, no CAD hits, no
frames for N seconds), "occupied" (CAD hits or frames), and "unreadable but present" (energy above
the floor that never correlates — i.e. a non-LoRa interferer). Anything stronger would be a claim
the hardware cannot support.

Listening costs nothing in regulatory terms: the duty cycle constrains transmission, not reception.

## 5. What the latency numbers actually say, and what we already do about them

The published ceiling for this medium is not subtle. A measured MeshCore link at 100 feet on the
fastest preset gave twelve matched command/reply pairs with a **median of 2.75 s and a maximum of
20.33 s**, 33 % of replies arriving after the client had already given up, and 15 of 27 commands
never answered at all. The Things Network's own documentation lists real-time data as "not
suitable for LoRaWAN". A ham-band SSH-over-RNode experiment reached 2.6 kbps at SF8 and reported
that "from about 5.7 kbps, interactive apps like htop or nano start to work better" — on a band
with no duty cycle at all.

Against that, what this project already avoids is the larger part of the problem:

- **No pull-based retrieval.** MeshCore's companion protocol pushes only `MSG_WAITING` and requires
  `CMD_SYNC_NEXT_MESSAGE` with a 5 s timeout and three retries — up to twenty seconds per message.
  Our profile pushes the screen.
- **No CTF-style full repaints.** The mesh profile sends plain-text rows and then only changed rows;
  a full screen is one screen, not a VT stream.
- **No idle chatter.** The ARQ heartbeat is now off (`heartbeat_ms: 0`), which removes ~1,200
  records an hour of pure cost.
- **Coalescing on the host.** A burst is one update per `BURST_GAP` (1 s), while a single change
  after quiet and the echo of typed input are sent immediately.

The remaining honest statement is that **a keystroke round trip on this link is bounded below by
two record airtimes plus the ack delay**, and that at 500 kHz / SF7 that is a few hundred
milliseconds, not tens. Interactive typing works because the echo is local and optimistic; a
full-screen program does not, because its updates are wholesale. Any future claim about latency
should be measured with `scripts/mesh_watch.py` and the host's `LDMUX_MESH_TIMING=1` lines rather
than estimated.

## 6. The five things worth doing next, in order

1. **A channel survey the gateway can run**: noise floor, CAD rate at our parameters, and
   `GetStats` deltas over a chosen dwell, reported as the three-way answer above. It answers "is
   someone else here" without claiming a guarantee, and it is the only way to pick a quiet preset
   other than guessing.
2. **An airtime figure that is measured, not assumed.** `mesh status` already carries the gateway's
   own estimate (`txAirtimeTenths`); add our own per-hour cumulative on-time so the European 1 %
   case can be checked rather than hoped for.
3. **Say which regime the preset implies.** A preset that is legal in the US at 500 kHz is not a
   preset that is legal in Europe at 1 %; the settings screen can state the band, the width and what
   that means, without pretending to know the user's jurisdiction.
4. **Keep the 500 kHz / SF7 preset as the default**, because it is both the fastest and the
   regulation-compatible choice where the radios are used.
5. **Do not add LBT to the fast path.** A CCA before every record costs 160 µs plus a retry window
   and buys nothing on a band with no duty cycle and two endpoints a metre apart; it belongs to the
   European sub-1 % configuration, if that is ever used.
