#!/usr/bin/env bash
# Live check of the voice extension inside a REAL pi runtime (not the fake API of test/*.test.ts).
# Throwaway everything under /tmp/voice-live; PI_CODING_AGENT_DIR points there, so ~/.pi is untouched.
# A stub OpenAI-compatible model server answers with a `say` fence only if the system prompt carries
# the voice section; a stub daemon socket records the `speech` RPC line it receives.
# Usage: test/live/live.sh        (needs `pi` on PATH; no network, no real model)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"; W=/tmp/voice-live
rm -rf "$W"; mkdir -p "$W"/{agent,work,sessions,prov,thrower}
cp "$here/stub-provider.ts" "$W/prov/index.ts"; cp "$here/thrower.ts" "$W/thrower/index.ts"
node "$here/stub-llm.mjs" 18777 & L=$!; node "$here/stub-daemon.mjs" "$W/d.sock" & D=$!
trap 'kill $L $D 2>/dev/null' EXIT; sleep 1
env_on=(LDMUX=1 LDMUX_SOCKET="$W/d.sock" LDMUX_SESSION_ID=s1)
echo "--- control: a throwing extension must be reported (a clean start then means something)"
env "${env_on[@]}" node "$here/rpc.mjs" "$W/work" '["-e","'"$W"'/thrower"]' '{"type":"get_state"}' 2>&1 | grep -q "CONTROL: deliberate load failure" && echo "control ok"
echo "--- on, prompt, off, prompt"
env "${env_on[@]}" node "$here/rpc.mjs" "$W/work" '[]' '{"id":"c1","type":"prompt","message":"/voice on"}' '{"id":"p1","type":"prompt","message":"hello"}' '{"id":"c2","type":"prompt","message":"/voice off"}' '{"id":"p2","type":"prompt","message":"again"}' >/dev/null 2>&1
n=$(wc -l < "$W/daemon-rpc.jsonl"); [ "$n" = 1 ] && echo "speech RPCs while on: 1 (ok)" || { echo "FAIL: $n speech RPCs"; exit 1; }
python3 - "$W/llm-requests.jsonl" <<'PY'
import json,sys
f=[ 'VOICE MODE IS ON' in l for l in open(sys.argv[1])]
assert f==[True,False], f; print("voice section per request:", f, "(ok)")
PY
echo "--- outside ldmux: inert"
rm -f "$W/daemon-rpc.jsonl"
env -u LDMUX -u LDMUX_SOCKET -u LDMUX_SESSION_ID node "$here/rpc.mjs" "$W/work" '[]' '{"id":"p","type":"prompt","message":"hello"}' >/dev/null 2>&1
[ ! -s "$W/daemon-rpc.jsonl" ] && echo "no speech RPC outside ldmux (ok)"
