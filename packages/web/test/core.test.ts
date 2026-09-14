import { test, expect } from "bun:test";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";

// core.js is a plain browser script (no modules, no build step): it attaches
// `globalThis.LdmuxCore`. Importing it here exercises exactly what the client
// downloads. Non-literal specifier keeps TS from requiring a .d.ts for it.
const coreUrl = new URL("../public/core.js", import.meta.url).href;
await import(coreUrl);
const core = (globalThis as any).LdmuxCore;

const enc = new TextEncoder();
const dec = new TextDecoder();

function frame(type: number, payload: string): Uint8Array {
  const body = enc.encode(payload);
  const out = new Uint8Array(core.HEADER_SIZE + body.length);
  const view = new DataView(out.buffer);
  view.setUint8(0, type);
  view.setUint8(1, 0);
  view.setUint16(2, 0);
  view.setUint32(4, body.length);
  out.set(body, core.HEADER_SIZE);
  return out;
}

test("decodes one complete output frame", () => {
  const { frames, tail, poisoned } = core.decodeFrames(new Uint8Array(0), frame(core.OUTPUT_FRAME, "hello"));
  expect(frames.length).toBe(1);
  expect(frames[0].type).toBe(core.OUTPUT_FRAME);
  expect(dec.decode(frames[0].payload)).toBe("hello");
  expect(tail.length).toBe(0);
  expect(poisoned).toBe(false);
});

test("decodes several frames from one chunk", () => {
  const chunk = new Uint8Array([...frame(core.OUTPUT_FRAME, "a"), ...frame(core.OUTPUT_FRAME, "bc")]);
  const { frames } = core.decodeFrames(new Uint8Array(0), chunk);
  expect(frames.map((f: any) => dec.decode(f.payload))).toEqual(["a", "bc"]);
});

test("reassembles a frame split across messages via the tail", () => {
  const whole = frame(core.OUTPUT_FRAME, "split-me");
  const first = core.decodeFrames(new Uint8Array(0), whole.subarray(0, 5));
  expect(first.frames.length).toBe(0);
  expect(first.tail.length).toBe(5);
  const second = core.decodeFrames(first.tail, whole.subarray(5));
  expect(second.frames.length).toBe(1);
  expect(dec.decode(second.frames[0].payload)).toBe("split-me");
  expect(second.tail.length).toBe(0);
});

test("keeps a partial second frame in the tail", () => {
  const a = frame(core.OUTPUT_FRAME, "one");
  const b = frame(core.OUTPUT_FRAME, "two-two");
  const chunk = new Uint8Array([...a, ...b.subarray(0, 6)]);
  const { frames, tail } = core.decodeFrames(new Uint8Array(0), chunk);
  expect(frames.map((f: any) => dec.decode(f.payload))).toEqual(["one"]);
  expect(tail.length).toBe(6);
});

test("a zero-length frame is valid (empty payload)", () => {
  const { frames } = core.decodeFrames(new Uint8Array(0), frame(core.OUTPUT_FRAME, ""));
  expect(frames.length).toBe(1);
  expect(frames[0].payload.length).toBe(0);
});

test("an oversized length poisons the stream and resets the tail", () => {
  const bad = frame(core.OUTPUT_FRAME, "x");
  new DataView(bad.buffer).setUint32(4, 64 * 1024 * 1024);
  const { frames, tail, poisoned } = core.decodeFrames(new Uint8Array(0), bad);
  expect(poisoned).toBe(true);
  expect(frames.length).toBe(0);
  expect(tail.length).toBe(0);
});

test("does not mutate the caller's tail buffer", () => {
  const tail = frame(core.OUTPUT_FRAME, "carry");
  const before = tail.slice();
  core.decodeFrames(tail, frame(core.OUTPUT_FRAME, "next"));
  expect([...tail]).toEqual([...before]);
});

test("scroll mode: bottom is following, above is anchored", () => {
  expect(core.scrollMode(10, 10)).toBe("following");
  expect(core.scrollMode(12, 10)).toBe("following");
  expect(core.scrollMode(9, 10)).toBe("anchored");
});

test("anchor survives a repaint and stands down when the line is gone", () => {
  const fromBottom = core.anchorFromBottom("anchored", 7, 10);
  expect(fromBottom).toBe(3);
  expect(core.anchorFromBottom("following", 7, 10)).toBe(0);
  // Same buffer extent: the anchored line is still there.
  expect(core.anchoredTarget(10, fromBottom)).toBe(7);
  // Buffer shrank (scrollback evicted): never guess a wrong line.
  expect(core.anchoredTarget(2, fromBottom)).toBe(-1);
});

test("cursor sequence is 1-based", () => {
  expect(core.cursorSequence(0, 0)).toBe("\x1b[1;1H");
  expect(core.cursorSequence(79, 23)).toBe("\x1b[24;80H");
  expect(core.cursorSequence(undefined, undefined)).toBe("\x1b[1;1H");
});

test("escapeHtml neutralises every markup character", () => {
  expect(core.escapeHtml(`<img src=x onerror="a">&'`)).toBe("&lt;img src=x onerror=&quot;a&quot;&gt;&amp;&#39;");
});

test("token comes from ?token=, #token=, or storage, in that order", () => {
  expect(core.tokenFrom("https://h/?token=abc", "", "stored").token).toBe("abc");
  expect(core.tokenFrom("https://h/", "#token=hash", "stored").token).toBe("hash");
  expect(core.tokenFrom("https://h/", "", "stored").token).toBe("stored");
  expect(core.tokenFrom("not a url", "", "").token).toBe("");
  // The cleaned path lets the client strip the secret from the address bar.
  expect(core.tokenFrom("https://h/path?token=abc", "", "").path).toBe("/path");
});

test("app.js is wired to the shared core (no duplicated decoder)", () => {
  const app = readFileSync(fileURLToPath(new URL("../public/app.js", import.meta.url)), "utf8");
  expect(app.includes("LdmuxCore")).toBe(true);
  expect(app.includes("decodeFrames")).toBe(true);
  // The native shell pins each pager page to one session with ?session=<id>.
  expect(app.includes("urlParam(\"session\")")).toBe(true);
  expect(app.includes("if (wantedSession) attach(wantedSession)")).toBe(true);
  expect(app.includes("function escapeHtml")).toBe(false);
  expect(app.includes("MAX_FRAME_PAYLOAD =")).toBe(false);
  // Every field interpolated into an innerHTML string must go through
  // escapeHtml: kind was the one omission (stored from an unvalidated param).
  expect(app.includes("${escapeHtml(s.kind)}")).toBe(true);
  expect(app.includes('class="s-kind">${s.kind}')).toBe(false);
  expect(app.includes("${escapeHtml(s.title || s.id)}")).toBe(true);
  expect(app.includes("${escapeHtml(s.status)}")).toBe(true);
  expect(app.includes("${escapeHtml(s.cwd || \"\")}")).toBe(true);
  const html = readFileSync(fileURLToPath(new URL("../public/index.html", import.meta.url)), "utf8");
  const coreAt = html.indexOf("/core.js");
  const appAt = html.indexOf("/app.js");
  expect(coreAt).toBeGreaterThan(-1);
  expect(coreAt).toBeLessThan(appAt);
});
test("unread markers compare activity against the last look", () => {
  expect(core.isUnread(2000, 1000)).toBe(true);
  expect(core.isUnread(1000, 1000)).toBe(false);
  expect(core.isUnread(1000, 2000)).toBe(false);
  // Unknown activity is not "new".
  expect(core.isUnread(undefined, 0)).toBe(false);
  expect(core.isUnread(0, 0)).toBe(false);
  // Never looked at: any real activity is unread.
  expect(core.isUnread(12345, undefined)).toBe(true);
});

test("the client wires history paging, unread dots, and the symbol page", () => {
  const app = readFileSync(fileURLToPath(new URL("../public/app.js", import.meta.url)), "utf8");
  expect(app.includes('type: "scrollback"')).toBe(true);
  expect(app.includes("isUnread(")).toBe(true);
  expect(app.includes("SYMBOLS")).toBe(true);
  // Every key button is built through the labelled/aria-aware helper.
  expect(app.includes("aria-label")).toBe(true);
  const html = readFileSync(fileURLToPath(new URL("../public/index.html", import.meta.url)), "utf8");
  expect(html.includes('id="history"')).toBe(true);
});

test("the service worker precaches the core and revalidates assets", () => {
  const sw = readFileSync(fileURLToPath(new URL("../public/sw.js", import.meta.url)), "utf8");
  expect(sw.includes('"/core.js"')).toBe(true);
  // Cache-first with no revalidation pinned stale clients to an old app.js.
  expect(sw.includes("stale-while-revalidate")).toBe(true);
  expect(sw.includes("refreshed")).toBe(true);
  expect(sw.includes('url.pathname.startsWith("/ws")')).toBe(true);
});

test("the shipped client is plain JavaScript (no type annotations)", () => {
  // app.js is loaded directly by the browser; a stray TS annotation is a
  // syntax error there and must be caught here.
  const app = readFileSync(fileURLToPath(new URL("../public/app.js", import.meta.url)), "utf8");
  const core = readFileSync(fileURLToPath(new URL("../public/core.js", import.meta.url)), "utf8");
  expect(app.includes(": Record<")).toBe(false);
  expect(app.includes(": string")).toBe(false);
  expect(core.includes(": string")).toBe(false);
  expect(app.includes("interface ")).toBe(false);
});

test("terminal modes are replayed from the snapshot (ADR-0004 fidelity)", () => {
  const all = { mouse: true, bracketedPaste: true, appCursor: true };
  const seq = core.modeSequences(all, true);
  expect(seq).toContain("\x1b[?2004h"); // bracketed paste
  expect(seq).toContain("\x1b[?1h"); // application cursor keys
  expect(seq).toContain("\x1b[?1000h"); // mouse button events
  expect(seq).toContain("\x1b[?1006h"); // SGR coordinates

  // Opt-in mouse: the pane wants it, the user has not asked for it.
  const noMouse = core.modeSequences(all, false);
  expect(noMouse).toContain("\x1b[?2004h");
  expect(noMouse).toContain("\x1b[?1000l");
  expect(noMouse).not.toContain("\x1b[?1000h");

  // A plain shell: every mode explicitly off (never left over from before).
  expect(core.modeSequences({}, true)).toBe("\x1b[?2004l\x1b[?1l\x1b[?1000l\x1b[?1002l\x1b[?1006l");
  expect(core.modeSequences(undefined, true)).toContain("\x1b[?2004l");
});

test("stale snapshots are detected so a late capture cannot undo an attach", () => {
  expect(core.isStaleSnapshot(3, 5)).toBe(true);
  expect(core.isStaleSnapshot(5, 5)).toBe(false);
  expect(core.isStaleSnapshot(6, 5)).toBe(false);
  // Unknown epochs carry no ordering information: never stale.
  expect(core.isStaleSnapshot(undefined, 5)).toBe(false);
  expect(core.isStaleSnapshot(3, undefined)).toBe(false);
  expect(core.isStaleSnapshot(0, 0)).toBe(false);
});

test("app.js replays modes, tracks epochs, and gates mouse behind a toggle", () => {
  const app = readFileSync(fileURLToPath(new URL("../public/app.js", import.meta.url)), "utf8");
  expect(app.includes("modeSequences(")).toBe(true);
  expect(app.includes("isStaleSnapshot(")).toBe(true);
  expect(app.includes('localStorage.getItem("ldmux.mouse")')).toBe(true);
  expect(app.includes("paneModes")).toBe(true);
  // The mouse toggle must not clobber the other modes.
  expect(app.includes("modeSequences(paneModes, mouseEnabled)")).toBe(true);
  const html = readFileSync(fileURLToPath(new URL("../public/index.html", import.meta.url)), "utf8");
  expect(html.includes('id="mouse"')).toBe(true);
});

test("switching sessions clears the previous session's modes", () => {
  const app = readFileSync(fileURLToPath(new URL("../public/app.js", import.meta.url)), "utf8");
  const attach = app.slice(app.indexOf("function attach("), app.indexOf("function loadHistory"));
  // A leftover MOUSE mode would take drags away from the new session's scroll.
  expect(attach).toContain("paneModes = {");
  expect(attach).toContain("paneHasMouse = false");
  expect(attach).toContain('els.mouse.classList.add("hidden")');
  expect(attach).toContain("lastEpoch = 0");
});

test("an attach always re-baselines the epoch, so a recreated stream cannot look stale", () => {
  // The server's per-stream counter restarts at 1 when a stream is recreated;
  // the client must therefore treat caps/attach as a new baseline rather than
  // comparing the new epoch against the old one and ignoring every snapshot.
  const app = readFileSync(fileURLToPath(new URL("../public/app.js", import.meta.url)), "utf8");
  const caps = app.slice(app.indexOf('case "caps"'), app.indexOf('case "snapshot"'));
  expect(caps).toContain("lastEpoch = Number(msg.snapshotEpoch");
  // and the guard must be per-session as well as per-epoch
  const snap = app.slice(app.indexOf('case "snapshot"'), app.indexOf('case "error"'));
  expect(snap).toContain("isStaleSnapshot(");
  expect(snap).toContain("msg.sessionId !== currentId");
});

test("decodeFrames accepts a plain array chunk (WebSocket may hand one over)", () => {
  const { frames } = core.decodeFrames([], Array.from(frame(core.OUTPUT_FRAME, "array")));
  expect(frames.length).toBe(1);
  expect(new TextDecoder().decode(frames[0].payload)).toBe("array");
});

test("modeSequences(null) disables every mode rather than throwing", () => {
  expect(core.modeSequences(null, true)).toBe("\x1b[?2004l\x1b[?1l\x1b[?1000l\x1b[?1002l\x1b[?1006l");
  // null vs undefined vs {}: all three are "no modes set".
  expect(core.modeSequences(undefined, true)).toBe(core.modeSequences({}, true));
});
