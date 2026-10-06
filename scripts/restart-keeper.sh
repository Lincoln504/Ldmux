#!/bin/sh
# Replace the running keeper with the installed binary.
#
# The keeper outlives the daemon, so nothing else replaces it and a deploy leaves it
# running old code (see scripts/install.sh). Killing it is the only way to load new
# keeper code, and it ENDS EVERY SHELL the keeper owns: that is the keeper's documented
# single point of failure (plan R3), not a bug. The daemon notices the dead socket on
# its next tick and auto-spawns the new binary (the one beside the daemon).
#
# The keeper is found by its process NAME (/proc/*/comm, exact match): matching a path
# on the command line can hit the daemon's own and kill the wrong thing (AGENTS.md).
# /proc, not pgrep, so it needs no procps.
set -eu

keeper_pid() {
  for c in /proc/[0-9]*/comm; do
    if [ "$(cat "$c" 2>/dev/null || true)" = ldmux-keeper ]; then
      p=${c#/proc/}; echo "${p%/comm}"; return 0
    fi
  done
  return 0
}

pid=$(keeper_pid)
if [ -z "$pid" ]; then
  echo "no keeper is running (the daemon starts one on demand)"
  exit 0
fi
echo "ending the keeper (pid $pid) and every shell it owns"
kill "$pid" 2>/dev/null || true
tries=0
while [ "$tries" -lt 40 ]; do
  sleep 0.25
  new=$(keeper_pid)
  if [ -n "$new" ] && [ "$new" != "$pid" ]; then
    echo "keeper restarted: pid $new"
    exit 0
  fi
  tries=$((tries + 1))
done
echo "the keeper is gone but the daemon has not respawned one yet" >&2
echo "check: journalctl --user -u ldmux -n 40" >&2
exit 1
