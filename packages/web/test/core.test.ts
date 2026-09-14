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
  expect(app.includes("function escapeHtml")).toBe(false);
  expect(app.includes("MAX_FRAME_PAYLOAD =")).toBe(false);
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
