// Drive pi --mode rpc: send commands from argv JSON list, print every record (trimmed).
import { spawn } from "node:child_process";
const [,, cwd, extra, ...cmds] = process.argv;
const args = ["--mode","rpc","--no-extensions","-e","/tmp/voice-live/prov/index.ts","-e",new URL("../../index.ts", import.meta.url).pathname,...JSON.parse(extra),"--model","stub/stub-1","--session-dir","/tmp/voice-live/sessions","--no-skills","--no-context-files","--no-mcp","--offline"];
const p = spawn("pi", args, { cwd, env: { ...process.env, PI_CODING_AGENT_DIR: "/tmp/voice-live/agent", PI_OFFLINE: "1" } });
p.stderr.on("data", d => console.log("STDERR:", String(d).trim()));
let buf = ""; const events = [];
p.stdout.on("data", d => { buf += d; let i; while ((i = buf.indexOf("\n")) >= 0) { const l = buf.slice(0, i); buf = buf.slice(i + 1); try { const r = JSON.parse(l); events.push(r); const s = JSON.stringify(r); console.log("REC:", s.length > 300 ? s.slice(0,300)+"…" : s); } catch { console.log("RAW:", l); } } });
const sleep = ms => new Promise(r => setTimeout(r, ms));
await sleep(3000);
for (const c of cmds) { p.stdin.write(c + "\n"); await sleep(4500); }
p.stdin.end(); await sleep(1000); p.kill();
