#!/usr/bin/env python3
"""Run the whole Rust test suite with one process per test binary, concurrently.

`cargo test --workspace` runs test binaries **one at a time**, so the wall clock
is the sum of every binary's time — about two minutes here, of which five
binaries are most of it. The binaries are independent: each uses its own scratch
state directory and asks the OS for a port (`0`), and the keeper reaper matches
its keeper by exact argv. So they can run at the same time, and the wall clock
becomes the slowest binary instead of the sum.

This is what `cargo nextest` does. It is not installed here and cannot be
(network is blocked), so this script does the one thing that matters: build with
`cargo test --no-run`, read the executables out of the JSON, run them in a pool,
and report a summary. `cargo test` remains the authority on pass/fail; this only
changes how the same binaries are scheduled.

    scripts/test-parallel.py                    # the gate
    scripts/test-parallel.py -p ldmux-daemon     # one crate
    scripts/test-parallel.py -- --ignored        # the slow tier
    LDMUX_TEST_JOBS=2 scripts/test-parallel.py   # bounded parallelism

Exit code is non-zero if any binary failed. A failing binary's output is printed
in full; a passing one only prints its `test result` line.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass


@dataclass
class Binary:
    name: str
    exe: str
    cwd: str


def build(extra: list[str]) -> tuple[list[Binary], float]:
    """Compile every test target and return the executables with their crate dir."""
    argv = ["cargo", "test", "--no-run", "--message-format=json", *extra]
    t0 = time.monotonic()
    proc = subprocess.run(argv, capture_output=True, text=True)
    build_s = time.monotonic() - t0
    if proc.returncode != 0:
        # A compile error: the JSON stream has the details in human form only on
        # stderr, and the summary is what the user needs first.
        sys.stderr.write(proc.stderr)
        sys.exit(proc.returncode)
    binaries: list[Binary] = []
    seen: set[str] = set()
    for line in proc.stdout.splitlines():
        try:
            msg = json.loads(line)
        except json.JSONDecodeError:
            continue
        if msg.get("reason") != "compiler-artifact":
            continue
        target = msg.get("target") or {}
        exe = msg.get("executable")
        if not target.get("test") or not exe:
            continue
        # Cargo reports each bin twice: once as the real binary and once as its
        # unit-test harness (`name-hash`). Only the harness runs tests; the real
        # one prints its usage and exits 2.
        if os.path.basename(exe) == target.get("name"):
            continue
        # The manifest's directory is the crate root, which is the working
        # directory `cargo test` would have used.
        cwd = os.path.dirname(msg.get("manifest_path", "")) or os.getcwd()
        key = os.path.basename(exe)
        if key in seen:
            continue
        seen.add(key)
        name = target.get("name", key)
        binaries.append(Binary(name=f"{name} ({key})", exe=exe, cwd=cwd))
    return binaries, build_s


def run_one(b: Binary, args: list[str], threads: int) -> tuple[Binary, int, float, str]:
    t0 = time.monotonic()
    argv = [b.exe, *args]
    # Every binary defaults to `--test-threads` = the core count, so a pool of
    # binaries oversubscribes the machine by its width. Spreading the cores over
    # the pool is the whole point of running them together.
    if threads > 0 and "--test-threads" not in " ".join(args):
        argv.append(f"--test-threads={threads}")
    proc = subprocess.run(argv, cwd=b.cwd, capture_output=True, text=True)
    took = time.monotonic() - t0
    return b, proc.returncode, took, proc.stdout + proc.stderr


def main() -> int:
    ap = argparse.ArgumentParser(add_help=True)
    ap.add_argument("-p", "--package", action="append", default=[], help="limit to a crate")
    ap.add_argument("-j", "--jobs", type=int, default=0, help="binaries in flight (default: CPUs)")
    ap.add_argument(
        "--threads",
        type=int,
        default=0,
        help="test threads per binary (default: cores / jobs)",
    )
    ap.add_argument(
        "--timeout",
        type=int,
        default=600,
        help="seconds before one binary is declared hung (default 600)",
    )
    ap.add_argument("rest", nargs="*", help="after `--`: arguments for each test binary")
    ns = ap.parse_args()

    # argparse cannot tell us where our own flags end and the binary's begin
    # without a separator, so split on the first `--` in argv instead.
    argv = sys.argv[1:]
    extra: list[str] = []
    bin_args: list[str] = []
    if "--" in argv:
        cut = argv.index("--")
        extra, bin_args = argv[:cut], argv[cut + 1 :]
    else:
        extra = argv
    # Strip our own flags out of `extra`, leaving cargo flags.
    cargo_flags: list[str] = []
    jobs = ns.jobs
    timeout = ns.timeout
    threads_flag = ns.threads
    i = 0
    while i < len(extra):
        a = extra[i]
        if a in ("-p", "--package"):
            cargo_flags += [a, extra[i + 1]]
            i += 2
        elif a in ("-j", "--jobs"):
            jobs = int(extra[i + 1])
            i += 2
        elif a == "--timeout":
            timeout = int(extra[i + 1])
            i += 2
        elif a == "--threads":
            threads_flag = int(extra[i + 1])
            i += 2
        elif a.startswith("--timeout="):
            timeout = int(a.split("=", 1)[1])
            i += 1
        elif a.startswith("-j") and a[2:].isdigit():
            jobs = int(a[2:])
            i += 1
        else:
            cargo_flags.append(a)
            i += 1
    del ap  # flags were parsed by hand above

    # Half the cores, not all of them: the binaries themselves spawn keepers,
    # shells and flood generators, so a pool at the core count oversubscribes the
    # machine (measured: 37 s at twelve, 31 s at six) and leaves nothing for the
    # editor the developer is iterating in.
    cores = os.cpu_count() or 4
    jobs = jobs or int(os.environ.get("LDMUX_TEST_JOBS") or 0) or max(4, cores // 2)
    binaries, build_s = build(cargo_flags)
    if not binaries:
        print("test-parallel: nothing to run", file=sys.stderr)
        return 1
    print(
        f"test-parallel: {len(binaries)} binaries, {jobs} at a time "
        f"(build {build_s:.1f} s)",
        flush=True,
    )

    t0 = time.monotonic()
    failures: list[tuple[Binary, str]] = []
    results: list[tuple[Binary, float, int, str]] = []
    lock = threading.Lock()

    threads = threads_flag or max(1, cores // max(1, jobs))
    if bin_args:
        threads = 0

    def one(b: Binary) -> None:
        try:
            r = run_one(b, bin_args, threads)
        except subprocess.TimeoutExpired:
            r = (b, -1, float(timeout), f"timed out after {timeout} s")
        with lock:
            results.append((r[0], r[2], r[1], r[3]))
            if r[1] != 0:
                failures.append((r[0], r[3]))
            mark = "ok" if r[1] == 0 else "FAIL"
            print(f"  {r[2]:6.2f} s  {mark:<4}  {b.name}", flush=True)

    # A hung binary must not wedge the pool forever.
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        list(pool.map(one, binaries))

    wall = time.monotonic() - t0
    results.sort(key=lambda r: -r[1])
    print()
    print(f"test-parallel: {len(binaries)} binaries in {wall:.1f} s (slowest first)")
    for b, took, _rc, _out in results[:5]:
        print(f"  {took:6.2f} s  {b.name}")
    # Doc tests are built and run one crate at a time by cargo itself and cannot
    # be launched as binaries, so they stay sequential. They cost about a second
    # and are part of what `cargo test --workspace` checks, so the gate runs them.
    t1 = time.monotonic()
    doc = subprocess.run(
        ["cargo", "test", "--doc", *cargo_flags], capture_output=True, text=True
    )
    doc_s = time.monotonic() - t1
    if doc.returncode != 0:
        print(doc.stdout + doc.stderr)
        failures.append((Binary("doc tests", "", os.getcwd()), doc.stdout + doc.stderr))
    else:
        print(f"  {doc_s:6.2f} s  ok    doc tests", flush=True)

    if failures:
        print()
        for b, out in failures:
            print(f"===== FAILED: {b.name} =====")
            print(out)
        return 1
    print("test-parallel: all binaries passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
