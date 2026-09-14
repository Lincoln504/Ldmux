import { test, expect } from "bun:test";
import { createSanitizer, sanitizeOnce } from "../src/sanitize.ts";

test("passes plain text and SGR through", () => {
  expect(sanitizeOnce("hello \x1b[31mred\x1b[0m world")).toBe("hello \x1b[31mred\x1b[0m world");
});

test("drops OSC 52 clipboard writes (BEL and ST terminated)", () => {
  expect(sanitizeOnce("a\x1b]52;c;SGVsbG8=\x07b")).toBe("ab");
  expect(sanitizeOnce("a\x1b]52;c;SGVsbG8=\x1b\\b")).toBe("ab");
});

test("keeps other OSC: titles and hyperlinks", () => {
  expect(sanitizeOnce("\x1b]0;title\x07")).toBe("\x1b]0;title\x07");
  expect(sanitizeOnce("x\x1b]8;;https://e\x1b\\link\x1b]8;;\x1b\\y")).toBe("x\x1b]8;;https://e\x1b\\link\x1b]8;;\x1b\\y");
});

test("drops DCS/SOS/PM/APC strings", () => {
  expect(sanitizeOnce("a\x1bPq#0;2;0;0;0\x1b\\b")).toBe("ab");
  expect(sanitizeOnce("a\x1b_Ga=T\x1b\\b")).toBe("ab");
});

test("drops device-status queries (the reason xterm.js would auto-reply)", () => {
  for (const query of ["\x1b[c", "\x1b[>c", "\x1b[0c", "\x1b[>0c", "\x1b[5n", "\x1b[6n", "\x1b[?0n"]) {
    expect(sanitizeOnce(`a${query}b`)).toBe("ab");
  }
});

test("keeps ordinary CSI sequences", () => {
  expect(sanitizeOnce("\x1b[2J\x1b[H\x1b[1;31m!\x1b[0m")).toBe("\x1b[2J\x1b[H\x1b[1;31m!\x1b[0m");
});

test("keeps two-character escapes", () => {
  expect(sanitizeOnce("a\x1b7b")).toBe("a\x1b7b");
});

test("reassembles a filtered sequence split across chunks", () => {
  const s = createSanitizer();
  const out = s.push("a\x1b") + s.push("]52;c;QQ==") + s.push("\x07") + s.push("b");
  expect(out).toBe("ab");
});

test("reassembles an allowed sequence split across chunks", () => {
  const s = createSanitizer();
  const out = s.push("a\x1b[3") + s.push("1m") + s.push("b");
  expect(out).toBe("a\x1b[31mb");
});

test("a malformed stream is bounded and never dumps its tail as text", () => {
  const s = createSanitizer();
  const out = s.push("\x1b[" + "1;".repeat(9000) + "m");
  expect(out).toBe("");
  // … and it resyncs at the next ESC rather than staying poisoned.
  expect(s.push("\x1b[32mok")).toBe("\x1b[32mok");
});
test("drops device-status queries in ANY parameter form, not just an allowlist", () => {
  // The policy keys on the final byte: a real terminal answers all of these, and
  // the answer would be sent back into the pane as input.
  for (const query of ["\x1b[?6n", "\x1b[1;2c", "\x1b[?62;1;2c", "\x1b[=c", "\x1b[?15n", "\x1b[999n"]) {
    expect(sanitizeOnce(`a${query}b`)).toBe("ab");
  }
});

test("still keeps CSI sequences whose final byte is not a device query", () => {
  // c/n are queries; p/q/J/m/H and friends are not and must survive intact.
  for (const keep of ["\x1b[0m", "\x1b[2J", "\x1b[3;7H", "\x1b[?25l", "\x1b[?25h", "\x1b[4h"]) {
    expect(sanitizeOnce(`a${keep}b`)).toBe(`a${keep}b`);
  }
});

test("reset() drops a partial sequence so a mode change cannot splice bytes", () => {
  // PaneStream resets the sanitizer on the control<->polling transition: a
  // pending partial sequence left by one source must not be joined to the
  // generated repaint bytes of the other.
  const s = createSanitizer();
  expect(s.push("a\x1b[3")).toBe("a");
  s.reset();
  expect(s.push("1m\x1b[2J")).toBe("1m\x1b[2J");
});
