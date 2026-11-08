#!/usr/bin/env python3
"""Synthesise the four themes' session cues as WAV files.

Run from `packages/mobile`:

    python3 tools/make-theme-sounds.py

Deterministic, stdlib only, no downloads: every tone is arithmetic, so the files can be rebuilt on
any machine and a diff in a WAV means a diff in this script. Output goes to
`composeApp/src/androidMain/res/raw/<theme>_<cue>.wav`, which is what `CuePlayer` reads.

The three cues are the session events the app actually reports — a link coming up, a link going
down, and something that needs the operator. Each theme answers in its own voice:

  forest      warm and wooden: a sine with a fast attack and a short decay, low register
  air         bright and airy: two sines a few cents apart, high register, long gentle decay
  daybreak    dry and precise: a short square-ish blip, mid register
  nerv        hard and lo-fi: a square wave, low, with stepped notes

Dark is absent on purpose: it is the quiet theme, and silence there is the design (see
`LdmuxTheme.sounds` and `Cues.kt`).

Every file is 22050 Hz, mono, 16-bit PCM, at most 0.4 s, peak 0.6 of full scale, with a 5 ms fade at
each end so neither end clicks. The script checks all of that and exits non-zero if a file it wrote
is out of spec, so a bad edit here fails loudly instead of shipping a pop.
"""

from __future__ import annotations

import math
import os
import struct
import sys
import wave

RATE = 22_050
PEAK = 0.6
MAX_SECONDS = 0.4
FADE_SECONDS = 0.005
OUT_DIR = os.path.join(
    os.path.dirname(os.path.dirname(os.path.abspath(__file__))),
    "composeApp", "src", "androidMain", "res", "raw",
)

# Note frequencies, equal temperament, A4 = 440.
def note(name: str) -> float:
    names = {"C": 0, "C#": 1, "D": 2, "D#": 3, "E": 4, "F": 5, "F#": 6, "G": 7, "G#": 8, "A": 9, "A#": 10, "B": 11}
    letter = name[:-1]
    octave = int(name[-1])
    semis = names[letter] + (octave - 4) * 12 - 9
    return 440.0 * (2.0 ** (semis / 12.0))


def sine(freq: float, t: float, detune: float = 0.0) -> float:
    return math.sin(2.0 * math.pi * freq * (1.0 + detune) * t)


def square(freq: float, t: float) -> float:
    # A band-limited-ish square: the first four odd harmonics, which is enough to read as "beep"
    # and cheap enough to keep this file readable.
    v = 0.0
    for k, amp in ((1, 1.0), (3, 1 / 3), (5, 1 / 5), (7, 1 / 7)):
        v += amp * math.sin(2.0 * math.pi * freq * k * t)
    return v * 0.7


def envelope(i: int, n: int, attack: float, decay: float) -> float:
    """Attack then exponential decay, both in samples, normalised to the segment length."""
    if i < attack:
        return i / max(attack, 1.0)
    return math.exp(-(i - attack) / max(decay, 1.0))


def place(buf: list[float], start_s: float, dur_s: float, render) -> None:
    """Add `render(t)` over `dur_s` starting at `start_s`."""
    start = int(start_s * RATE)
    n = int(dur_s * RATE)
    for i in range(n):
        idx = start + i
        if idx >= len(buf):
            break
        buf[idx] += render(i, n)


def fade(buf: list[float]) -> None:
    n = int(FADE_SECONDS * RATE)
    for i in range(min(n, len(buf))):
        buf[i] *= i / n
        buf[-1 - i] *= i / n


def write(name: str, buf: list[float]) -> tuple[str, float, int, int]:
    """Scale to [PEAK], write, and report what actually went to disk.

    The peak is measured from the *file*, not from the buffer: a scale factor that clipped would
    otherwise report a perfect 0.6 while the samples sat on the rail.
    """
    loudest = max((abs(v) for v in buf), default=0.0) or 1.0
    scale = PEAK / loudest
    samples = [int(max(-1.0, min(1.0, v * scale)) * 32767) for v in buf]
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
    return path, len(written) / RATE, max(abs(v) for v in written) / 32767.0, clipped


# --- the themes -------------------------------------------------------------------------------

def forest(cue: str) -> list[float]:
    """Warm, wooden, low: a sine with a fast attack and a 0.25 s decay."""
    def strike(freq: float):
        return lambda i, n: envelope(i, n, 0.004 * RATE, 0.25 * RATE) * sine(freq, i / RATE)

    buf = [0.0] * int(0.40 * RATE)
    if cue == "link_up":
        # A rising third: the smallest interval that reads as an answer rather than a repeat.
        place(buf, 0.00, 0.22, strike(note("F#3")))
        place(buf, 0.11, 0.26, strike(note("A#3")))
    elif cue == "link_down":
        place(buf, 0.00, 0.26, strike(note("A#3")))
        place(buf, 0.10, 0.28, strike(note("D#3")))
    else:  # attention
        place(buf, 0.00, 0.14, strike(note("C3")))
        place(buf, 0.18, 0.20, strike(note("C3")))
    return buf


def air(cue: str) -> list[float]:
    """Bright and airy: a sine pair a few cents apart, high register, long decay."""
    def glass(freq: float):
        return lambda i, n: envelope(i, n, 0.003 * RATE, 0.30 * RATE) * (
            sine(freq, i / RATE, detune=0.0) + 0.5 * sine(freq, i / RATE, detune=0.004)
        ) * 0.7

    buf = [0.0] * int(0.40 * RATE)
    if cue == "link_up":
        place(buf, 0.00, 0.16, glass(note("A5")))
        place(buf, 0.09, 0.30, glass(note("E6")))
    elif cue == "link_down":
        place(buf, 0.00, 0.16, glass(note("E6")))
        place(buf, 0.09, 0.30, glass(note("A5")))
    else:
        place(buf, 0.00, 0.14, glass(note("C6")))
        place(buf, 0.16, 0.24, glass(note("C6")))
    return buf


def daybreak(cue: str) -> list[float]:
    """Dry and precise: a short square-ish blip, mid register."""
    def blip(freq: float):
        return lambda i, n: envelope(i, n, 0.002 * RATE, 0.075 * RATE) * square(freq, i / RATE)

    buf = [0.0] * int(0.32 * RATE)
    if cue == "link_up":
        place(buf, 0.00, 0.09, blip(note("G4")))
        place(buf, 0.10, 0.14, blip(note("D5")))
    elif cue == "link_down":
        place(buf, 0.00, 0.09, blip(note("D5")))
        place(buf, 0.10, 0.14, blip(note("G4")))
    else:
        place(buf, 0.00, 0.06, blip(note("A4")))
        place(buf, 0.08, 0.10, blip(note("A4")))
    return buf


def nerv(cue: str) -> list[float]:
    """Hard, lo-fi, low: a square wave with stepped notes, the sound of a terminal that means it."""
    def beep(freq: float, decay: float = 0.05):
        return lambda i, n: envelope(i, n, 0.002 * RATE, decay * RATE) * square(freq, i / RATE)

    buf = [0.0] * int(0.34 * RATE)
    if cue == "link_up":
        place(buf, 0.00, 0.07, beep(note("A3")))
        place(buf, 0.08, 0.07, beep(note("C#4")))
        place(buf, 0.16, 0.14, beep(note("E4"), decay=0.09))
    elif cue == "link_down":
        place(buf, 0.00, 0.07, beep(note("E4")))
        place(buf, 0.08, 0.07, beep(note("C#4")))
        place(buf, 0.16, 0.14, beep(note("A3"), decay=0.09))
    else:
        # A minor second apart: dissonant on purpose, because this one is a complaint.
        place(buf, 0.00, 0.08, beep(note("A3")))
        place(buf, 0.10, 0.12, beep(note("A#3"), decay=0.07))
    return buf


def gruvbox(cue: str) -> list[float]:
    """Warm and retro: one soft square note with a step, the colour of a 1980s machine's ready tone."""
    def tone(freq: float, decay: float = 0.06):
        return lambda i, n: envelope(i, n, 0.003 * RATE, decay * RATE) * square(freq, i / RATE)

    buf = [0.0] * int(0.30 * RATE)
    if cue == "link_up":
        place(buf, 0.00, 0.10, tone(note("F4")))
        place(buf, 0.12, 0.12, tone(note("A4"), decay=0.08))
    elif cue == "link_down":
        place(buf, 0.00, 0.10, tone(note("A4")))
        place(buf, 0.12, 0.12, tone(note("F4"), decay=0.08))
    else:
        place(buf, 0.00, 0.08, tone(note("C4")))
        place(buf, 0.09, 0.08, tone(note("C4")))
    return buf


THEMES = {
    "forest": forest,
    "air": air,
    "daybreak": daybreak,
    "nerv": nerv,
    "gruvbox": gruvbox,
}
CUES = ("link_up", "link_down", "attention")


def main() -> int:
    os.makedirs(OUT_DIR, exist_ok=True)
    failures: list[str] = []
    for theme, render in THEMES.items():
        for cue in CUES:
            buf = render(cue)
            fade(buf)
            path, seconds, peak, clipped = write(f"{theme}_{cue}", buf)
            ok = seconds <= MAX_SECONDS and abs(peak - PEAK) < 0.02 and clipped == 0
            print(
                f"{os.path.basename(path):34s} {seconds * 1000:6.1f} ms  peak {peak:.2f}  "
                f"clipped {clipped}  {'ok' if ok else 'OUT OF SPEC'}"
            )
            if not ok:
                failures.append(os.path.basename(path))
    if failures:
        print(f"\n{len(failures)} file(s) out of spec: {', '.join(failures)}", file=sys.stderr)
        return 1
    print(f"\n{len(THEMES) * len(CUES)} cues written to {os.path.relpath(OUT_DIR)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
