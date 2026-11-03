#!/usr/bin/env python3
"""Sample the mesh link and report what changed, so a "drop" becomes a measurement.

    scripts/mesh_watch.py              # 2 s samples, for ever
    scripts/mesh_watch.py -n 60 -i 2   # 60 samples
    scripts/mesh_watch.py --json       # one sample, the daemon's own JSON

Why this exists: "the connection keeps dropping" is not a report anyone can act on. The
daemon already carries every number needed to say what kind of drop it was, and they are
only visible as deltas against a previous reading:

  retx   rising while nothing is being sent  -> the peer is not answering (link loss)
  retx   rising while sending                -> records are being lost on the air
  state  waiting -> established -> waiting   -> rehandshakes, i.e. a session dying
  pending > 0 and flat                       -> the window is full and the peer is silent
  budgetStalls rising                        -> the radio's hourly budget is exhausted

Reads `ldmux mesh status --json`, which is a local RPC: no radio traffic is generated.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys
import time

FIELDS = ("state", "sent", "receivedBytes", "retransmits", "dropped", "pending", "budgetStalls")


def sample() -> dict | None:
    try:
        out = subprocess.run(
            ["ldmux", "mesh", "status", "--json"],
            capture_output=True,
            text=True,
            timeout=10,
        ).stdout
        return json.loads(out)
    except (subprocess.SubprocessError, json.JSONDecodeError, OSError):
        return None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("-n", "--samples", type=int, default=0, help="0 means for ever")
    ap.add_argument("-i", "--interval", type=float, default=2.0)
    ap.add_argument("--json", action="store_true", help="print one sample and exit")
    args = ap.parse_args()

    if args.json:
        v = sample()
        if v is None:
            print("no answer from the daemon", file=sys.stderr)
            return 1
        print(json.dumps(v, indent=2, sort_keys=True))
        return 0

    prev: dict | None = None
    prev_session: dict = {}
    n = 0
    first = True
    print(f"{'time':8}  {'state':11}  {'sent':>5} {'recv':>5} {'retx':>5} {'pend':>4}  note")
    while args.samples == 0 or n < args.samples:
        v = sample()
        if v is None:
            print(f"{time.strftime('%H:%M:%S'):8}  ---- no answer from the daemon ----")
        else:
            s = v.get("session") or {}
            cur = {k: s.get(k, 0) for k in FIELDS[1:]}
            cur["state"] = v.get("state")
            note = []
            if prev is None:
                note.append("first sample")
            else:
                d = {k: cur[k] - prev[k] for k in FIELDS[1:]}
                if d["retransmits"] and not d["sent"] and not d["receivedBytes"]:
                    note.append("retransmitting with nothing moving: peer silent")
                if d["retransmits"] and d["sent"]:
                    note.append("records are being lost on the air")
                if cur["pending"] and prev["pending"] == cur["pending"]:
                    note.append("window full and flat")
                if d["budgetStalls"]:
                    note.append("hourly budget exhausted")
            if prev is not None and s.get("state") != prev_session.get("state"):
                note.append(f"session {prev_session.get('state')} -> {s.get('state')}")
            err = v.get("lastError") or s.get("lastError")
            if err:
                note.append(f"last error: {err}")
            print(
                f"{time.strftime('%H:%M:%S'):8}  {cur['state']:<11}  "
                f"{cur['sent']:5} {cur['receivedBytes']:5} {cur['retransmits']:5} "
                f"{cur['pending']:4}  {'; '.join(note)}"
            )
            prev = cur
            prev_session = s
        n += 1
        if first:
            first = False
        if args.samples == 0 or n < args.samples:
            time.sleep(args.interval)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
