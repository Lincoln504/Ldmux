import { test, expect } from "../../testing/src/expect.ts";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { byteCapacity, encodeQr, renderQrPbm, renderQrTerminal, type Ecc } from "../src/qr.ts";
import { run } from "../src/exec.ts";

/**
 * The encoder is judged by a decoder, not by its own bits.
 *
 * Every version at every error-correction level, each filled to capacity, is
 * rendered and read back by zbar (pinned in the devShell). A wrong table entry,
 * a misplaced format bit or a bad interleave makes a code that does not scan, and
 * comparing against a second encoder would only prove the two agree.
 *
 * zbar missing is a failure, not a skip: a test that skips when its oracle is
 * absent passes on exactly the machine where nothing was checked.
 */

const ECCS: Ecc[] = ["L", "M", "Q", "H"];

/** Printable ASCII, varied, so no two codes share a payload. */
function payload(len: number, seed: number): string {
  const alphabet = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789:/?&=.-_%~";
  let x = seed * 2654435761 >>> 0;
  let s = "";
  for (let i = 0; i < len; i++) {
    x = (x * 1103515245 + 12345) >>> 0;
    s += alphabet[(x >>> 16) % alphabet.length];
  }
  return s;
}

test("every version and error-correction level decodes back to its payload", { timeout: 120000 }, async () => {
  const dir = mkdtempSync(join(tmpdir(), "ldmux-qr-"));
  try {
    const cases: { file: string; text: string }[] = [];
    for (let ver = 1; ver <= 40; ver++) {
      for (const ecc of ECCS) {
        const text = payload(byteCapacity(ver, ecc), ver * 4 + ECCS.indexOf(ecc));
        const code = encodeQr(text, ecc);
        // Filled to capacity, so it must land on exactly this version and level.
        expect(`${code.version}${code.ecc}`).toBe(`${ver}${ecc}`);
        const file = join(dir, `v${ver}${ecc}.pbm`);
        writeFileSync(file, renderQrPbm(code, 4, ver > 20 ? 2 : 4));
        cases.push({ file, text });
      }
    }
    // One zbar run for all of them; it reports in file order.
    const res = await run(["zbarimg", "--raw", "-q", "-Sdisable", "-Sqrcode.enable", ...cases.map((c) => c.file)], {
      timeoutMs: 100000,
    });
    if (res.code === 127 || /ENOENT|not found/.test(res.err)) {
      throw new Error("zbarimg is not on PATH: run the tests inside `nix develop`");
    }
    expect(res.code).toBe(0);
    const decoded = res.out.split("\n").filter((l) => l.length > 0);
    expect(decoded).toEqual(cases.map((c) => c.text));
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("a code is the smallest version that fits, at the strongest level that fits it", () => {
  // 20 bytes: version 1 at L holds 17, so version 2 — which holds 32 at L, 26 at
  // M, 20 at Q and 14 at H, so it is boosted to Q.
  expect(byteCapacity(1, "L")).toBe(17);
  expect([byteCapacity(2, "L"), byteCapacity(2, "M"), byteCapacity(2, "Q"), byteCapacity(2, "H")]).toEqual([32, 26, 20, 14]);
  const code = encodeQr("x".repeat(20), "L");
  expect(code.version).toBe(2);
  expect(code.ecc).toBe("Q");
  expect(code.size).toBe(25);
});

test("too long for any version is an error, not a truncated code", () => {
  expect(() => encodeQr("x".repeat(byteCapacity(40, "L") + 1))).toThrow(/too long/);
});

test("the terminal rendering is two modules per line, dark on light, quiet zone included", () => {
  const code = encodeQr("ldmux", "L");
  const lines = renderQrTerminal(code).split("\n");
  // 21 modules + 4 + 4 quiet = 29 rows, two per line.
  expect(lines).toHaveLength(15);
  // Explicit colours only — never the terminal's default foreground.
  expect(lines[0]).toContain("\x1b[38;5;231;48;5;231m");
  expect(lines.every((l) => l.endsWith("\x1b[0m"))).toBe(true);
  expect(lines.join("")).toContain("\x1b[38;5;16;");
});

/**
 * The terminal rendering, read back the way a camera sees it.
 *
 * Each character cell is one module wide and two tall, its top half the
 * foreground colour and its bottom half the background. Parsing the escapes back
 * into pixels and decoding those proves the half-block scheme, the colour choice
 * and the quiet zone together — not just that the bits were right before drawing.
 */
test("what ldmux code prints decodes back to the pairing link", { timeout: 30000 }, async () => {
  const link = "ldmux://pair?v=1&host=100.127.119.54:7625&token=" + "0123456789abcdef".repeat(4) + "&name=debian";
  const lines = renderQrTerminal(encodeQr(link)).split("\n");
  const cell = /\x1b\[38;5;(\d+);48;5;(\d+)m\u2580/g;
  const rows: string[] = [];
  for (const line of lines) {
    const top: string[] = [];
    const bottom: string[] = [];
    for (const m of line.matchAll(cell)) {
      top.push(m[1] === "16" ? "1" : "0");
      bottom.push(m[2] === "16" ? "1" : "0");
    }
    rows.push(top.join(""), bottom.join(""));
  }
  const width = rows[0]!.length;
  const scale = 4;
  const px = rows.flatMap((r) => Array(scale).fill([...r].map((b) => b.repeat(scale)).join("")));
  const dir = mkdtempSync(join(tmpdir(), "ldmux-qrterm-"));
  try {
    const file = join(dir, "term.pbm");
    writeFileSync(file, `P1\n${width * scale} ${px.length}\n${px.join("\n")}\n`);
    const res = await run(["zbarimg", "--raw", "-q", "-Sdisable", "-Sqrcode.enable", file]);
    expect(res.out.trim()).toBe(link);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
