/*
 * The cell flag word is one wire format with two definitions: the C core writes
 * it, Kotlin reads it. A bit that means "selected" on one side and something else
 * on the other is a silent corruption, and neither language can see the other's
 * constant, so the agreement has to be checked from outside both — here.
 */
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");

function read(rel: string): string {
  return readFileSync(join(root, rel), "utf8");
}

/** The bit number C shifts for a flag, e.g. `LDMUX_FLAG_SELECTED (1u << 9)`. */
function cFlagBit(header: string, name: string): number {
  const m = header.match(new RegExp(`#define\\s+${name}\\s+\\(1u\\s*<<\\s*(\\d+)\\)`));
  assert.ok(m, `${name} not found in the C header`);
  return Number(m[1]);
}

/** The bit number Kotlin shifts for a flag, e.g. `const val FLAG_SELECTED = 1 shl 9`. */
function kotlinFlagBit(source: string, name: string): number {
  const m = source.match(new RegExp(`const val\\s+${name}\\s*=\\s*1\\s+shl\\s+(\\d+)`));
  assert.ok(m, `${name} not found in the Kotlin source`);
  return Number(m[1]);
}

test("selection flag bit agrees between the C core and the client", () => {
  const header = read("native/ldmux_terminal.h");
  const frame = read("composeApp/src/commonMain/kotlin/dev/ldmux/mobile/term/TerminalFrame.kt");
  assert.equal(cFlagBit(header, "LDMUX_FLAG_SELECTED"), kotlinFlagBit(frame, "FLAG_SELECTED"));
});

test("every style flag bit agrees between the C core and the client", () => {
  // Bit by bit, not a sample. The bits a client ignores are harmless; the bits it
  // *misreads* are not, and the one that made the cursor invisible in `pi` was a
  // mismatch nobody had checked. Both sides declare their bits now — the C used to
  // shift them inline in the encoder, which is why this could only assert on the
  // block's boundary before.
  const header = read("native/ldmux_terminal.h");
  const frame = read("composeApp/src/commonMain/kotlin/dev/ldmux/mobile/term/TerminalFrame.kt");
  const pairs: [string, string][] = [
    ["LDMUX_FLAG_BOLD", "FLAG_BOLD"],
    ["LDMUX_FLAG_ITALIC", "FLAG_ITALIC"],
    ["LDMUX_FLAG_UNDERLINE", "FLAG_UNDERLINE"],
    ["LDMUX_FLAG_FAINT", "FLAG_FAINT"],
    ["LDMUX_FLAG_BLINK", "FLAG_BLINK"],
    ["LDMUX_FLAG_INVERSE", "FLAG_INVERSE"],
    ["LDMUX_FLAG_INVISIBLE", "FLAG_INVISIBLE"],
    ["LDMUX_FLAG_STRIKETHROUGH", "FLAG_STRIKETHROUGH"],
    ["LDMUX_FLAG_OVERLINE", "FLAG_OVERLINE"],
    ["LDMUX_FLAG_SELECTED", "FLAG_SELECTED"],
    ["LDMUX_FLAG_WIDE", "FLAG_WIDE"],
    ["LDMUX_FLAG_SPACER_TAIL", "FLAG_SPACER_TAIL"],
    ["LDMUX_FLAG_HYPERLINK", "FLAG_HYPERLINK"],
  ];
  for (const [cName, ktName] of pairs) {
    assert.equal(cFlagBit(header, cName), kotlinFlagBit(frame, ktName), `${ktName} disagrees`);
  }
  // ...and the style block stays clear of the selection bit, so adding a flag to
  // one side without a free bit cannot silently overlap the other.
  const styleBits = pairs.slice(0, 9).map(([cName]) => cFlagBit(header, cName));
  assert.deepEqual([...styleBits].sort((a, b) => a - b), [0, 1, 2, 3, 4, 5, 6, 7, 8]);
  assert.equal(cFlagBit(header, "LDMUX_FLAG_SELECTED"), 9);
  // The v1 additions sit in the bits that were spare, around the 3-bit underline
  // style field at 12..14, and nothing overlaps.
  assert.equal(cFlagBit(header, "LDMUX_FLAG_WIDE"), 10);
  assert.equal(cFlagBit(header, "LDMUX_FLAG_SPACER_TAIL"), 11);
  assert.equal(cFlagBit(header, "LDMUX_FLAG_HYPERLINK"), 15);
});

test("the underline style field, frame version and cluster cap agree", () => {
  const header = read("native/ldmux_terminal.h");
  const frame = read("composeApp/src/commonMain/kotlin/dev/ldmux/mobile/term/TerminalFrame.kt");
  const num = (src: string, re: RegExp, what: string): number => {
    const m = src.match(re);
    assert.ok(m, `${what} not found`);
    return Number(m[1]);
  };
  assert.equal(
    num(header, /#define\s+LDMUX_UNDERLINE_STYLE_SHIFT\s+(\d+)/, "C style shift"),
    num(frame, /const val\s+UNDERLINE_STYLE_SHIFT\s*=\s*(\d+)/, "Kotlin style shift"),
  );
  assert.equal(
    num(header, /#define\s+LDMUX_UNDERLINE_STYLE_MASK\s+(\d+)u?/, "C style mask"),
    num(frame, /const val\s+UNDERLINE_STYLE_MASK\s*=\s*(\d+)/, "Kotlin style mask"),
  );
  assert.equal(
    num(header, /#define\s+LDMUX_FRAME_VERSION\s+(\d+)/, "C version"),
    num(frame, /const val\s+FRAME_VERSION\s*=\s*(\d+)/, "Kotlin version"),
  );
  assert.equal(
    num(header, /#define\s+LDMUX_CELL_MAX_UTF8\s+(\d+)/, "C cluster cap"),
    num(frame, /const val\s+CELL_MAX_UTF8\s*=\s*(\d+)/, "Kotlin cluster cap"),
  );
  assert.equal(
    num(header, /#define\s+LDMUX_CELL_FIXED\s+(\d+)/, "C fixed record"),
    num(frame, /const val\s+CELL_FIXED\s*=\s*(\d+)/, "Kotlin fixed record"),
  );
  // The u8 wire length bounds the cap.
  assert.ok(num(header, /#define\s+LDMUX_CELL_MAX_UTF8\s+(\d+)/, "cap") <= 255);
});

test("the frame header size agrees", () => {
  // The same shape of mistake as the flags, and the newest one: the header grew
  // from 40 to 48 bytes for the cursor byte, and a one-sided move would decode the
  // first cell record as the last header byte without either test suite noticing.
  const header = read("native/ldmux_terminal.h");
  const frame = read("composeApp/src/commonMain/kotlin/dev/ldmux/mobile/term/TerminalFrame.kt");
  const c = header.match(/#define\s+LDMUX_FRAME_HEADER\s+(\d+)/);
  assert.ok(c, "LDMUX_FRAME_HEADER not found in the C header");
  const kt = frame.match(/const val\s+HEADER_SIZE\s*=\s*(\d+)/);
  assert.ok(kt, "HEADER_SIZE not found in the Kotlin source");
  assert.equal(Number(c[1]), Number(kt[1]));
});
