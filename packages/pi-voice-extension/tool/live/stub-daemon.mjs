import { createServer } from "node:net";
import { appendFileSync, rmSync } from "node:fs";
const path = process.argv[2]; try { rmSync(path); } catch {}
createServer(s => { s.setEncoding("utf8"); let b = ""; s.on("data", c => { b += c; if (b.includes("\n")) { appendFileSync("/tmp/voice-live/daemon-rpc.jsonl", b.split("\n")[0] + "\n"); s.write(JSON.stringify({ id: 1, ok: true, result: { seq: 1, delivered: 1 } }) + "\n"); } }); }).listen(path);
