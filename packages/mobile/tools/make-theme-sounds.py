#!/usr/bin/env python3
"""Synthesise the five themes' session cues as WAV files.

Run from `packages/mobile`:

    python3 tools/make-theme-sounds.py

Deterministic, stdlib only, no downloads: every tone is arithmetic, so the files can be rebuilt on
any machine and a diff in a WAV means a diff in this script. Output goes to
`composeApp/src/androidMain/res/raw/<theme>_<cue>.wav`, which is what `CuePlayer` reads.

The three cues are the session events the app actually reports — a link coming up, a link going
down, and something that needs the operator — and each theme answers in **one material**, the way a
real instrument does, rather than in a set of unrelated beeps:

  forest      a struck wooden branch: inharmonic free-free bar modes, a short contact, a little
              filtered noise for the strike itself. Low and dry.
  air         a muted silver clink: the measured handbell partial ratios, a 2.5 Hz warble from the
              shape asymmetry, everything shortened hard rather than pitched down.
  daybreak    a dry electrical blip: a square with a 1.5 ms attack, no material at all, mid register.
  nerv        a hard terminal beep: gated sine at 1.9 kHz with a second partial at 3.6 kHz and a
              3 ms noise tick at the onset, held flat rather than decaying.
  umber       a paper tick and a retro two-note tone: band-passed noise with a 3.2 kHz resonance for
              the tick, a square pair for the tones.

Dark is absent on purpose: it is the quiet theme, and silence there is the design (see
`LdmuxTheme.sounds` and `Cues.kt`).

The synthesis choices come from the modal-synthesis literature rather than from taste:

- a struck object is a bank of exponentially decaying sines at the mode frequencies, with **higher
  partials decaying faster** (`d_n = ρ·f_n`), which is why a struck sound mellows as it rings;
- **partials start at phase zero**, so the summed waveform leaves zero without a fade-in, and only
  the tail gets the 5 ms fade that keeps a truncated decay from clicking;
- contact time sets brightness (≈1 ms puts the first null of the contact spectrum near 1 kHz), so
  wood is a ~1 ms strike and silver a ~0.4 ms one;
- nothing above 10 kHz matters for a cue, which is what makes 22.05 kHz mono honest rather than a
  compromise;
- every file is **peak-normalised to its own target**, because equal peak is not equal loudness: a
  2-5 kHz cue reads far louder than a low knock at the same peak, so the bell is trimmed relative to
  the wood.

Every file is 22050 Hz, mono, 16-bit PCM, at most 0.4 s, with a 5 ms tail fade and a DC blocker, and
the script checks all of that — duration, peak against its target, clipped samples, DC offset, and
that both ends start and finish near zero — exiting non-zero if any file is out of spec, so a bad
edit here fails loudly instead of shipping a pop.
"""

from __future__ import annotations

import math
import os
import random
import struct
import sys
import wave

RATE = 22_050
MAX_SECONDS = 0.4
FADE_SECONDS = 0.005
HEAD_SECONDS = 0.001
OUT_DIR = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "composeApp", "src", "androidMain", "res", "raw",
)

# Peak per theme, in full scale. A 2-5 kHz cue reads louder than a low knock at the same peak, so
# the silver bell sits 6 dB under the wood and the beep 4 dB under it; the paper tick is the quietest
# thing in the set by design.
PEAK_BY_THEME = {
    "forest": 0.71,
    "air": 0.50,
    "daybreak": 0.63,
    "nerv": 0.79,
    "umber": 0.66,
}


# --- elementary voices ------------------------------------------------------------------------


def note(name: str) -> float:
    """Equal temperament, A4 = 440."""
    names = {
        "C": 0, "C#": 1, "D": 2, "D#": 3, "E": 4, "F": 5,
        "F#": 6, "G": 7, "G#": 8, "A": 9, "A#": 10, "B": 11,
    }
    letter = name[:-1]
    octave = int(name[-1])
    semis = names[letter] + (octave - 4) * 12 - 9
    return 440.0 * (2.0 ** (semis / 12.0))


def square(freq: float, t: float) -> float:
    """A band-limited-ish square: the first four odd harmonics, enough to read as "beep"."""
    value = 0.0
    for k, amp in ((1, 1.0), (3, 1 / 3), (5, 1 / 5), (7, 1 / 7)):
        value += amp * math.sin(2.0 * math.pi * freq * k * t)
    return value * 0.7


def decay(seconds: float) -> float:
    """A T60 in seconds as an exponential time constant."""
    return seconds / 6.907755  # ln(1000): T60 is the time to fall 60 dB.


def modal(
    f0: float,
    ratios: list[float],
    t60_first: float,
    decay_mul: list[float] | None = None,
    warble_hz: float = 0.0,
    amp_mul: list[float] | None = None,
) -> list[float]:
    """One struck object: exponentially decaying sines at `f0 * ratios`, phase zero at t=0.

    Higher partials decay faster (`decay_mul[i] < 1` shortens their T60), which is the difference
    between a struck bar and an organ. A non-zero `warble_hz` adds a twin of the fundamental, the
    beating that a real asymmetric bell's first resonance shows (measured at 2.5 Hz on a handbell).
    """
    voices: list[tuple[float, float, float]] = []  # (frequency, tau, amplitude)
    for i, ratio in enumerate(ratios):
        freq = f0 * ratio
        if freq >= 0.45 * RATE:  # stay well inside Nyquist
            continue
        mul = decay_mul[i] if decay_mul and i < len(decay_mul) else 1.0
        amp = amp_mul[i] if amp_mul and i < len(amp_mul) else 1.0 / (1.0 + 0.6 * i)
        voices.append((freq, decay(t60_first * mul), amp))
    if warble_hz > 0.0:
        voices.append((f0 + warble_hz, decay(t60_first * 0.9), 0.6))
    # The longest decay decides the buffer, and 6 time constants is where a truncated tail is under
    # -52 dB — inaudible, so the fade has nothing left to hide.
    longest = max(tau for _, tau, _ in voices)
    n = int(min(MAX_SECONDS, 6.0 * longest) * RATE)
    out = [0.0] * n
    for freq, tau, amp in voices:
        omega = 2.0 * math.pi * freq / RATE
        for i in range(n):
            out[i] += amp * math.exp(-i / (tau * RATE)) * math.sin(omega * i)
    return out


def noise_burst(n: int, low_hz: float, high_hz: float, seed: int) -> list[float]:
    """White noise through a one-pole high-pass and a one-pole low-pass: the strike's contact noise."""
    rng = random.Random(seed)
    high = 1.0 / (1.0 + 2.0 * math.pi * high_hz / RATE)
    lp = 0.0
    prev = 0.0
    hp_alpha = 1.0 / (1.0 + 2.0 * math.pi * low_hz / RATE)
    out = []
    for _ in range(n):
        white = rng.uniform(-1.0, 1.0)
        lp += (1.0 - high) * (white - lp)
        prev = lp - hp_alpha * prev
        out.append(prev)
    return out


def mix_into(buf: list[float], start_s: float, part: list[float], gain: float = 1.0) -> None:
    start = int(start_s * RATE)
    for i, value in enumerate(part):
        idx = start + i
        if idx >= len(buf):
            break
        buf[idx] += value * gain


def strike(f0: float, ratios: list[float], t60: float, noise: float, seed: int, dur: float) -> list[float]:
    """A modal strike plus its contact noise, in one buffer."""
    core = modal(f0, ratios, t60, decay_mul=[1.0, 0.8, 0.65, 0.55, 0.45, 0.4])
    n = len(core)
    if noise > 0.0:
        tick = noise_burst(min(n, int(0.012 * RATE)), 150.0, 4_000.0, seed)
        longest = max(abs(v) for v in core) or 1.0
        loudest = max(abs(v) for v in tick) or 1.0
        for i, value in enumerate(tick):
            core[i] += value / loudest * longest * noise
    # Trim to the cue's own length, with the strike's attack untouched.
    return core[: min(n, int(dur * RATE))]


def beep(freq: float, dur: float, hold: float, tick: int) -> list[float]:
    """A gated sine with a second partial and a noise tick: the one voice that is not a material."""
    n = int(dur * RATE)
    attack = int(0.0015 * RATE)
    release = int(0.010 * RATE)
    out = [0.0] * n
    for i in range(n):
        t = i / RATE
        env = 1.0
        if i < attack:
            env = i / max(attack, 1)
        elif i > n - release:
            env = max(0.0, (n - i) / max(release, 1))
        held = min(1.0, max(0.0, (hold * RATE - i) / max(0.002 * RATE, 1.0))) if i > hold * RATE else 1.0
        env *= held
        out[i] = env * (math.sin(2.0 * math.pi * freq * t) + 0.25 * math.sin(2.0 * math.pi * freq * 1.9 * t))
    if tick:
        click = noise_burst(int(0.003 * RATE), 1_500.0, 9_000.0, tick)
        loudest = max(abs(v) for v in out) or 1.0
        for i, value in enumerate(click):
            if i < len(out):
                out[i] += value * loudest * 0.12
    return out


def paper_tick(seed: int) -> list[float]:
    """Paper is almost all transient: band-passed noise with a small tonal anchor."""
    n = int(0.030 * RATE)
    body = noise_burst(n, 1_200.0, 8_000.0, seed)
    # A resonant bump near 3.2 kHz, which is what makes paper read as crisp rather than as hiss.
    omega = 2.0 * math.pi * 3_200.0 / RATE
    tau = decay(0.020)
    for i in range(n):
        env = math.exp(-i / (tau * RATE))
        body[i] += 0.15 * env * math.sin(omega * i) + 0.08 * env * math.sin(2.0 * omega * i)
    fade_out = [v * math.exp(-i / (0.010 * RATE)) for i, v in enumerate(body)]
    return fade_out


# --- the themes -------------------------------------------------------------------------------


def forest(cue: str) -> list[float]:
    """A struck wooden branch: free-free bar ratios, ~1 ms contact, a little filtered noise."""
    # The bar series (1, 2.756, 5.404, 8.933 × f0) is what makes a knock read as wood rather than as
    # a drum: two of the partials are inharmonic, and they die fast.
    ratios = [1.0, 2.756, 5.404, 8.933]
    if cue == "link_up":
        buf = [0.0] * int(0.30 * RATE)
        mix_into(buf, 0.00, strike(note("A3"), ratios, 0.075, 0.4, 11, 0.12))
        mix_into(buf, 0.09, strike(note("E4"), ratios, 0.065, 0.4, 12, 0.12))
        return buf
    if cue == "link_down":
        buf = [0.0] * int(0.30 * RATE)
        mix_into(buf, 0.00, strike(note("E4"), ratios, 0.070, 0.4, 21, 0.12))
        mix_into(buf, 0.09, strike(note("A3"), ratios, 0.085, 0.4, 22, 0.13))
        return buf
    buf = [0.0] * int(0.32 * RATE)
    mix_into(buf, 0.00, strike(note("A3"), ratios, 0.070, 0.4, 31, 0.11))
    mix_into(buf, 0.13, strike(note("A3"), ratios, 0.060, 0.4, 32, 0.10))
    return buf


def air(cue: str) -> list[float]:
    """A muted silver clink: the handbell ratios, a 2.5 Hz warble, everything shortened hard."""
    # Measured handbell partials, normalised: 1.00, 1.80, 2.52, 2.99, 3.80, 4.56.
    ratios = [1.0, 1.80, 2.52, 2.99, 3.80, 4.56]
    if cue == "link_up":
        buf = [0.0] * int(0.40 * RATE)
        mix_into(buf, 0.00, modal(note("C6"), ratios, 0.30, warble_hz=2.5))
        mix_into(buf, 0.10, modal(note("G6"), ratios, 0.26, warble_hz=2.5))
        return buf
    if cue == "link_down":
        buf = [0.0] * int(0.40 * RATE)
        mix_into(buf, 0.00, modal(note("G6"), ratios, 0.30, warble_hz=2.5))
        mix_into(buf, 0.10, modal(note("C6"), ratios, 0.30, warble_hz=2.5))
        return buf
    buf = [0.0] * int(0.34 * RATE)
    mix_into(buf, 0.00, modal(note("C6"), ratios, 0.26, warble_hz=2.5))
    mix_into(buf, 0.14, modal(note("C6"), ratios, 0.18, warble_hz=2.5))
    return buf


def daybreak(cue: str) -> list[float]:
    """A dry electrical blip: square, 1.5 ms attack, no material, mid register."""
    def blip(freq: float, dur: float, seed: int) -> list[float]:
        out = beep(freq, dur, dur * 0.8, seed)
        # A square is the theme's voice, so modulate the sine with one for the body only.
        for i in range(len(out)):
            out[i] = 0.7 * square(freq, i / RATE) + 0.3 * out[i]
        return out

    if cue == "link_up":
        buf = [0.0] * int(0.30 * RATE)
        mix_into(buf, 0.00, blip(note("G4"), 0.09, 41))
        mix_into(buf, 0.10, blip(note("D5"), 0.13, 42))
        return buf
    if cue == "link_down":
        buf = [0.0] * int(0.30 * RATE)
        mix_into(buf, 0.00, blip(note("D5"), 0.09, 51))
        mix_into(buf, 0.10, blip(note("G4"), 0.13, 52))
        return buf
    buf = [0.0] * int(0.24 * RATE)
    mix_into(buf, 0.00, blip(note("A4"), 0.06, 61))
    mix_into(buf, 0.09, blip(note("A4"), 0.07, 62))
    return buf


def nerv(cue: str) -> list[float]:
    """A hard terminal beep: gated sine at 1.9 kHz, second partial, a 3 ms noise tick."""
    if cue == "link_up":
        buf = [0.0] * int(0.30 * RATE)
        mix_into(buf, 0.00, beep(1_760.0, 0.06, 0.045, 71))
        mix_into(buf, 0.07, beep(2_093.0, 0.06, 0.045, 72))
        mix_into(buf, 0.14, beep(2_637.0, 0.14, 0.110, 73))
        return buf
    if cue == "link_down":
        buf = [0.0] * int(0.30 * RATE)
        mix_into(buf, 0.00, beep(2_637.0, 0.06, 0.045, 81))
        mix_into(buf, 0.07, beep(2_093.0, 0.06, 0.045, 82))
        mix_into(buf, 0.14, beep(1_760.0, 0.14, 0.110, 83))
        return buf
    # A minor second apart: dissonant on purpose, because this one is a complaint.
    buf = [0.0] * int(0.26 * RATE)
    mix_into(buf, 0.00, beep(1_760.0, 0.08, 0.065, 91))
    mix_into(buf, 0.10, beep(1_863.0, 0.12, 0.100, 92))
    return buf


def umber(cue: str) -> list[float]:
    """A paper tick for attention, a retro two-note square pair for the link."""
    if cue == "attention":
        buf = [0.0] * int(0.09 * RATE)
        mix_into(buf, 0.00, paper_tick(101))
        mix_into(buf, 0.045, paper_tick(102))
        return buf
    if cue == "link_up":
        buf = [0.0] * int(0.30 * RATE)
        mix_into(buf, 0.00, beep(note("F4"), 0.10, 0.080, 111))
        mix_into(buf, 0.12, beep(note("A4"), 0.13, 0.100, 112))
        return buf
    buf = [0.0] * int(0.30 * RATE)
    mix_into(buf, 0.00, beep(note("A4"), 0.10, 0.080, 121))
    mix_into(buf, 0.12, beep(note("F4"), 0.13, 0.100, 122))
    return buf


THEMES = {
    "forest": forest,
    "air": air,
    "daybreak": daybreak,
    "nerv": nerv,
    "umber": umber,
}
CUES = ("link_up", "link_down", "attention")


# --- output -----------------------------------------------------------------------------------


def finish(buf: list[float], peak: float) -> tuple[list[float], float]:
    """DC-block, fade the tail, and peak-normalise to `peak`."""
    # A one-pole DC blocker at ~5 Hz: summing modes that start at phase zero leaves an offset, and an
    # offset is what actually clicks when a buffer edge is cut.
    alpha = 1.0 - 2.0 * math.pi * 5.0 / RATE
    prev_x = 0.0
    prev_y = 0.0
    for i, value in enumerate(buf):
        y = value - prev_x + alpha * prev_y
        prev_x, prev_y = value, y
        buf[i] = y
    # **The head gets 1 ms, not 5.** A strike has to keep its attack — a 5 ms fade-in measurably
    # softens one — but a modal sum that starts at phase zero still carries the contact noise's
    # first sample, and a buffer that begins at ±0.06 clicks. One millisecond is 22 samples: long
    # enough to leave zero, far too short to hear as a fade.
    head = min(int(HEAD_SECONDS * RATE), len(buf))
    for i in range(head):
        buf[i] *= i / head
    n = min(int(FADE_SECONDS * RATE), len(buf))
    for i in range(n):
        buf[-1 - i] *= i / n
    loudest = max((abs(v) for v in buf), default=0.0) or 1.0
    return [v * peak / loudest for v in buf], peak


def write(name: str, buf: list[float], peak: float) -> tuple[str, float, float, int, float, float, float]:
    """Write the WAV and report what actually went to disk, measured from the file."""
    scaled, _ = finish(list(buf), peak)
    samples = [int(max(-1.0, min(1.0, v)) * 32767) for v in scaled]
    frames = b"".join(struct.pack("<h", v) for v in samples)
    path = os.path.join(OUT_DIR, f"{name}.wav")
    with wave.open(path, "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(RATE)
        w.writeframes(frames)
    with wave.open(path, "rb") as w:
        assert w.getframerate() == RATE and w.getnchannels() == 1 and w.getsampwidth() == 2
        written = struct.unpack("<%dh" % w.getnframes(), w.readframes(w.getnframes()))
    clipped = sum(1 for v in written if abs(v) >= 32767)
    dc = sum(written) / len(written) / 32767.0
    return (
        path,
        len(written) / RATE,
        max(abs(v) for v in written) / 32767.0,
        clipped,
        dc,
        written[0] / 32767.0,
        written[-1] / 32767.0,
    )


def main() -> int:
    os.makedirs(OUT_DIR, exist_ok=True)
    failures: list[str] = []
    for theme, render in THEMES.items():
        peak_target = PEAK_BY_THEME[theme]
        for cue in CUES:
            path, seconds, peak, clipped, dc, first, last = write(f"{theme}_{cue}", render(cue), peak_target)
            problems = []
            if seconds > MAX_SECONDS + 1e-6:
                problems.append(f"{seconds:.3f} s is longer than {MAX_SECONDS}")
            if abs(peak - peak_target) > 0.02:
                problems.append(f"peak {peak:.2f} is not {peak_target:.2f}")
            if clipped:
                problems.append(f"{clipped} clipped samples")
            if abs(dc) > 0.002:
                problems.append(f"DC offset {dc:+.4f}")
            if abs(first) > 0.02 or abs(last) > 0.02:
                problems.append(f"edge samples {first:+.3f}/{last:+.3f}")
            status = "ok" if not problems else "OUT OF SPEC: " + "; ".join(problems)
            if problems:
                failures.append(f"{os.path.basename(path)}: {'; '.join(problems)}")
            print(
                f"{os.path.basename(path):34s} {seconds * 1000:6.1f} ms  peak {peak:.2f}  "
                f"dc {dc:+.4f}  clipped {clipped}  {status}"
            )
    written = len(THEMES) * len(CUES)
    print(f"\n{written} cues written to {os.path.relpath(OUT_DIR)}")
    if failures:
        for failure in failures:
            print(f"FAIL {failure}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
