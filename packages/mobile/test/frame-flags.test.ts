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
  ];
  for (const [cName, ktName] of pairs) {
    assert.equal(cFlagBit(header, cName), kotlinFlagBit(frame, ktName), `${ktName} disagrees`);
  }
  // ...and the style block stays clear of the selection bit, so adding a flag to
  // one side without a free bit cannot silently overlap the other.
  const styleBits = pairs.slice(0, 9).map(([cName]) => cFlagBit(header, cName));
  assert.deepEqual([...styleBits].sort((a, b) => a - b), [0, 1, 2, 3, 4, 5, 6, 7, 8]);
  assert.equal(cFlagBit(header, "LDMUX_FLAG_SELECTED"), 9);
});

test("the frame header size agrees", () => {
  // The same shape of mistake as the flags, and the newest one: the header grew
  // from 40 to 48 bytes for the cursor byte, and a one-sided move would decode the
  // first cell record as the last header byte without either test suite noticing.
  const header = read("native/ldmux_terminal.h");
  const frame = read("composeApp/src/commonMain/kotlin/dev/ldmux/mobile/term/TerminalFrame.kt");
  const c = header.match(/#define\s+FRAME_HEADER\s+(\d+)/);
  assert.ok(c, "FRAME_HEADER not found in the C header");
  const kt = frame.match(/const val\s+HEADER_SIZE\s*=\s*(\d+)/);
  assert.ok(kt, "HEADER_SIZE not found in the Kotlin source");
  assert.equal(Number(c[1]), Number(kt[1]));
});
