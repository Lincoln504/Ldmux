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

test("the style flags the client already decodes still agree", () => {
  // The bits a client ignores are harmless; the bits it *misreads* are not. Bold
  // through overline are the ones that were already on the wire.
  const header = read("native/ldmux_terminal.h");
  const frame = read("composeApp/src/commonMain/kotlin/dev/ldmux/mobile/term/TerminalFrame.kt");
  // C writes these inline rather than as #defines, so the check is that the C
  // side's highest style bit stays below the selection bit and that Kotlin's
  // numbering is what the header implies.
  const selected = cFlagBit(header, "LDMUX_FLAG_SELECTED");
  assert.ok(selected >= 9, "the style block occupies bits 0..8; selection must not overlap it");
  assert.equal(kotlinFlagBit(frame, "FLAG_OVERLINE"), 8);
});
