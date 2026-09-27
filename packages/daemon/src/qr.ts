/**
 * A QR code encoder, for `ldmux code`.
 *
 * Byte mode only (the payload is a URL), every version 1–40, every error
 * correction level. The algorithm is ISO/IEC 18004 as laid out by Project Nayuki's
 * reference implementation: encode, add Reed–Solomon codewords per block,
 * interleave, place, then choose the mask with the lowest penalty.
 *
 * Written here rather than taken as a dependency because `ws` is the only
 * production dependency (AGENTS.md) and no QR tool is on the pinned runtime.
 * Correctness is pinned by decoding every size back with zbar
 * (`test/qr.test.ts`), not by comparing against another encoder's bits.
 */

export type Ecc = "L" | "M" | "Q" | "H";

/** A square grid of modules; `true` is dark. `modules[y][x]`. */
export interface QrCode {
  version: number;
  ecc: Ecc;
  size: number;
  modules: boolean[][];
}

const ECC_ORDER: readonly Ecc[] = ["L", "M", "Q", "H"];
const FORMAT_BITS: Record<Ecc, number> = { L: 1, M: 0, Q: 3, H: 2 };

// Indexed by version (0 unused).
const ECC_CODEWORDS_PER_BLOCK: Record<Ecc, readonly number[]> = {
  L: [-1, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28, 30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
  M: [-1, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28],
  Q: [-1, 13, 22, 18, 26, 18, 24, 18, 22, 20, 24, 28, 26, 24, 20, 30, 24, 28, 28, 26, 30, 28, 30, 30, 30, 30, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
  H: [-1, 17, 28, 22, 16, 22, 28, 26, 26, 24, 28, 24, 28, 22, 24, 24, 30, 28, 28, 26, 28, 30, 24, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30],
};
const ECC_BLOCKS: Record<Ecc, readonly number[]> = {
  L: [-1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13, 14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25],
  M: [-1, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21, 23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49],
  Q: [-1, 1, 1, 2, 2, 4, 4, 6, 6, 8, 8, 8, 10, 12, 16, 12, 17, 16, 18, 21, 20, 23, 23, 25, 27, 29, 34, 34, 35, 38, 40, 43, 45, 48, 51, 53, 56, 59, 62, 65, 68],
  H: [-1, 1, 1, 2, 4, 4, 4, 5, 6, 8, 8, 11, 11, 16, 16, 18, 16, 19, 21, 25, 25, 25, 34, 30, 32, 35, 37, 40, 42, 45, 48, 51, 54, 57, 60, 63, 66, 70, 74, 77, 81],
};

/** Modules available for data and ECC codewords, after every function pattern. */
function rawDataModules(ver: number): number {
  let n = (16 * ver + 128) * ver + 64;
  if (ver >= 2) {
    const align = Math.floor(ver / 7) + 2;
    n -= (25 * align - 10) * align - 55;
    if (ver >= 7) n -= 36;
  }
  return n;
}

function dataCodewords(ver: number, ecc: Ecc): number {
  return Math.floor(rawDataModules(ver) / 8) - ECC_CODEWORDS_PER_BLOCK[ecc][ver]! * ECC_BLOCKS[ecc][ver]!;
}

/** The most bytes a byte-mode code of [ver] at [ecc] holds. */
export function byteCapacity(ver: number, ecc: Ecc): number {
  return Math.floor((dataCodewords(ver, ecc) * 8 - 4 - (ver <= 9 ? 8 : 16)) / 8);
}

/** Bits a byte-mode segment of [len] bytes needs at [ver]. */
function segmentBits(ver: number, len: number): number {
  return 4 + (ver <= 9 ? 8 : 16) + 8 * len;
}

// ── GF(256) and Reed–Solomon ────────────────────────────────────────────────

function gfMul(x: number, y: number): number {
  let z = 0;
  for (let i = 7; i >= 0; i--) {
    z = (z << 1) ^ ((z >>> 7) * 0x11d);
    z ^= ((y >>> i) & 1) * x;
  }
  return z;
}

function rsDivisor(degree: number): number[] {
  const result = new Array<number>(degree).fill(0);
  result[degree - 1] = 1;
  let root = 1;
  for (let i = 0; i < degree; i++) {
    for (let j = 0; j < degree; j++) {
      result[j] = gfMul(result[j]!, root);
      if (j + 1 < degree) result[j]! ^= result[j + 1]!;
    }
    root = gfMul(root, 0x02);
  }
  return result;
}

function rsRemainder(data: readonly number[], divisor: readonly number[]): number[] {
  const result = new Array<number>(divisor.length).fill(0);
  for (const b of data) {
    const factor = b ^ result.shift()!;
    result.push(0);
    for (let i = 0; i < divisor.length; i++) result[i]! ^= gfMul(divisor[i]!, factor);
  }
  return result;
}

/** Split into blocks, append each block's ECC, and interleave. */
function withEcc(data: readonly number[], ver: number, ecc: Ecc): number[] {
  const blocks = ECC_BLOCKS[ecc][ver]!;
  const eccLen = ECC_CODEWORDS_PER_BLOCK[ecc][ver]!;
  const raw = Math.floor(rawDataModules(ver) / 8);
  const shortBlocks = blocks - (raw % blocks);
  const shortLen = Math.floor(raw / blocks);
  const divisor = rsDivisor(eccLen);
  const all: number[][] = [];
  for (let i = 0, k = 0; i < blocks; i++) {
    const dat = data.slice(k, k + shortLen - eccLen + (i < shortBlocks ? 0 : 1));
    k += dat.length;
    const block = [...dat];
    // Short blocks carry a placeholder so every block has the same length;
    // it is skipped when interleaving.
    if (i < shortBlocks) block.push(0);
    all.push([...block, ...rsRemainder(dat, divisor)]);
  }
  const out: number[] = [];
  for (let i = 0; i < all[0]!.length; i++) {
    for (let j = 0; j < all.length; j++) {
      if (i !== shortLen - eccLen || j >= shortBlocks) out.push(all[j]![i]!);
    }
  }
  return out;
}

// ── Placement ───────────────────────────────────────────────────────────────

class Grid {
  size: number;
  modules: boolean[][];
  fixed: boolean[][];

  constructor(size: number) {
    this.size = size;
    this.modules = Array.from({ length: size }, () => new Array<boolean>(size).fill(false));
    this.fixed = Array.from({ length: size }, () => new Array<boolean>(size).fill(false));
  }

  set(x: number, y: number, dark: boolean): void {
    this.modules[y]![x] = dark;
    this.fixed[y]![x] = true;
  }
}

function alignmentPositions(ver: number, size: number): number[] {
  if (ver === 1) return [];
  const n = Math.floor(ver / 7) + 2;
  const step = Math.floor((ver * 8 + n * 3 + 5) / (n * 4 - 4)) * 2;
  const out = [6];
  for (let pos = size - 7; out.length < n; pos -= step) out.splice(1, 0, pos);
  return out;
}

function drawFunctionPatterns(g: Grid, ver: number): void {
  const size = g.size;
  for (let i = 0; i < size; i++) {
    g.set(6, i, i % 2 === 0);
    g.set(i, 6, i % 2 === 0);
  }
  for (const [cx, cy] of [[3, 3], [size - 4, 3], [3, size - 4]] as const) {
    for (let dy = -4; dy <= 4; dy++) {
      for (let dx = -4; dx <= 4; dx++) {
        const x = cx + dx;
        const y = cy + dy;
        if (x < 0 || x >= size || y < 0 || y >= size) continue;
        const d = Math.max(Math.abs(dx), Math.abs(dy));
        g.set(x, y, d !== 2 && d !== 4);
      }
    }
  }
  const pos = alignmentPositions(ver, size);
  const last = pos.length - 1;
  for (let i = 0; i < pos.length; i++) {
    for (let j = 0; j < pos.length; j++) {
      // The three corners already hold finder patterns.
      if ((i === 0 && j === 0) || (i === 0 && j === last) || (i === last && j === 0)) continue;
      for (let dy = -2; dy <= 2; dy++) {
        for (let dx = -2; dx <= 2; dx++) {
          g.set(pos[i]! + dx, pos[j]! + dy, Math.max(Math.abs(dx), Math.abs(dy)) !== 1);
        }
      }
    }
  }
  // Reserve the format areas now; the real bits go in once the mask is known.
  drawFormat(g, "L", 0);
  if (ver >= 7) {
    let rem = ver;
    for (let i = 0; i < 12; i++) rem = (rem << 1) ^ ((rem >>> 11) * 0x1f25);
    const bits = (ver << 12) | rem;
    for (let i = 0; i < 18; i++) {
      const dark = ((bits >>> i) & 1) !== 0;
      const a = size - 11 + (i % 3);
      const b = Math.floor(i / 3);
      g.set(a, b, dark);
      g.set(b, a, dark);
    }
  }
}

function drawFormat(g: Grid, ecc: Ecc, mask: number): void {
  const data = (FORMAT_BITS[ecc] << 3) | mask;
  let rem = data;
  for (let i = 0; i < 10; i++) rem = (rem << 1) ^ ((rem >>> 9) * 0x537);
  const bits = ((data << 10) | rem) ^ 0x5412;
  const bit = (i: number) => ((bits >>> i) & 1) !== 0;
  const size = g.size;
  for (let i = 0; i <= 5; i++) g.set(8, i, bit(i));
  g.set(8, 7, bit(6));
  g.set(8, 8, bit(7));
  g.set(7, 8, bit(8));
  for (let i = 9; i < 15; i++) g.set(14 - i, 8, bit(i));
  for (let i = 0; i < 8; i++) g.set(size - 1 - i, 8, bit(i));
  for (let i = 8; i < 15; i++) g.set(8, size - 15 + i, bit(i));
  g.set(8, size - 8, true);
}

function drawCodewords(g: Grid, data: readonly number[]): void {
  const size = g.size;
  let i = 0;
  for (let right = size - 1; right >= 1; right -= 2) {
    if (right === 6) right = 5;
    for (let vert = 0; vert < size; vert++) {
      for (let j = 0; j < 2; j++) {
        const x = right - j;
        const upward = ((right + 1) & 2) === 0;
        const y = upward ? size - 1 - vert : vert;
        if (!g.fixed[y]![x] && i < data.length * 8) {
          g.modules[y]![x] = ((data[i >>> 3]! >>> (7 - (i & 7))) & 1) !== 0;
          i++;
        }
      }
    }
  }
}

function maskHit(mask: number, x: number, y: number): boolean {
  switch (mask) {
    case 0: return (x + y) % 2 === 0;
    case 1: return y % 2 === 0;
    case 2: return x % 3 === 0;
    case 3: return (x + y) % 3 === 0;
    case 4: return (Math.floor(x / 3) + Math.floor(y / 2)) % 2 === 0;
    case 5: return ((x * y) % 2) + ((x * y) % 3) === 0;
    case 6: return (((x * y) % 2) + ((x * y) % 3)) % 2 === 0;
    default: return (((x + y) % 2) + ((x * y) % 3)) % 2 === 0;
  }
}

function applyMask(g: Grid, mask: number): void {
  for (let y = 0; y < g.size; y++) {
    for (let x = 0; x < g.size; x++) {
      if (!g.fixed[y]![x] && maskHit(mask, x, y)) g.modules[y]![x] = !g.modules[y]![x];
    }
  }
}

/** The four penalty rules of ISO/IEC 18004 §7.8.3. Lower scans better. */
function penalty(m: boolean[][]): number {
  const size = m.length;
  const at = (x: number, y: number, horizontal: boolean) => (horizontal ? m[y]![x]! : m[x]![y]!);
  let score = 0;
  // A finder-like run: dark light dark dark dark light dark, with four light on one side.
  const finder = [true, false, true, true, true, false, true];
  for (const horizontal of [true, false]) {
    for (let a = 0; a < size; a++) {
      let run = 1;
      for (let b = 1; b <= size; b++) {
        if (b < size && at(b, a, horizontal) === at(b - 1, a, horizontal)) {
          run++;
          continue;
        }
        if (run >= 5) score += 3 + (run - 5);
        run = 1;
      }
      for (let b = 0; b + 7 <= size; b++) {
        if (!finder.every((v, k) => at(b + k, a, horizontal) === v)) continue;
        const lightBefore = [1, 2, 3, 4].every((k) => b - k < 0 || !at(b - k, a, horizontal));
        const lightAfter = [0, 1, 2, 3].every((k) => b + 7 + k >= size || !at(b + 7 + k, a, horizontal));
        if (lightBefore || lightAfter) score += 40;
      }
    }
  }
  let dark = 0;
  for (let y = 0; y < size; y++) {
    for (let x = 0; x < size; x++) {
      if (m[y]![x]) dark++;
      if (x + 1 < size && y + 1 < size) {
        const c = m[y]![x];
        if (c === m[y]![x + 1] && c === m[y + 1]![x] && c === m[y + 1]![x + 1]) score += 3;
      }
    }
  }
  const total = size * size;
  score += (Math.ceil(Math.abs(dark * 20 - total * 10) / total) - 1) * 10;
  return score;
}

/**
 * Encode [text] as a QR code.
 *
 * The smallest version that holds it at [minEcc], then the strongest error
 * correction that still fits that version: a bigger code costs terminal rows,
 * and stronger correction at the same size is free.
 */
export function encodeQr(text: string, minEcc: Ecc = "L"): QrCode {
  const bytes = [...new TextEncoder().encode(text)];
  let ver = 0;
  for (let v = 1; v <= 40; v++) {
    if (segmentBits(v, bytes.length) <= dataCodewords(v, minEcc) * 8) {
      ver = v;
      break;
    }
  }
  if (ver === 0) throw new Error(`too long for a QR code: ${bytes.length} bytes`);
  let ecc = minEcc;
  for (const e of ECC_ORDER.slice(ECC_ORDER.indexOf(minEcc) + 1)) {
    if (segmentBits(ver, bytes.length) <= dataCodewords(ver, e) * 8) ecc = e;
  }

  const capacity = dataCodewords(ver, ecc) * 8;
  const bits: number[] = [];
  const push = (value: number, len: number) => {
    for (let i = len - 1; i >= 0; i--) bits.push((value >>> i) & 1);
  };
  push(0b0100, 4);
  push(bytes.length, ver <= 9 ? 8 : 16);
  for (const b of bytes) push(b, 8);
  push(0, Math.min(4, capacity - bits.length));
  push(0, (8 - (bits.length % 8)) % 8);
  const data: number[] = [];
  for (let i = 0; i < bits.length; i += 8) data.push(bits.slice(i, i + 8).reduce((acc, b) => (acc << 1) | b, 0));
  for (let pad = 0xec; data.length < capacity / 8; pad ^= 0xec ^ 0x11) data.push(pad);

  const size = ver * 4 + 17;
  const g = new Grid(size);
  drawFunctionPatterns(g, ver);
  drawCodewords(g, withEcc(data, ver, ecc));

  let best = -1;
  let bestScore = Infinity;
  for (let mask = 0; mask < 8; mask++) {
    applyMask(g, mask);
    drawFormat(g, ecc, mask);
    const s = penalty(g.modules);
    if (s < bestScore) {
      best = mask;
      bestScore = s;
    }
    applyMask(g, mask); // XOR again: undo.
  }
  applyMask(g, best);
  drawFormat(g, ecc, best);
  return { version: ver, ecc, size, modules: g.modules };
}

/**
 * Draw a code for a terminal: two modules per character cell, using the upper
 * half block with the top module as foreground and the bottom as background.
 *
 * The colours are explicit (xterm-256 16 and 231, pure black and pure white)
 * rather than "whatever this terminal's default is": a scanner wants dark
 * modules on light, and a dark-themed terminal drawing the default would hand it
 * the negative. Those two indices are in the fixed colour cube, which themes do
 * not remap the way they remap the first sixteen.
 */
export function renderQrTerminal(code: QrCode, quiet = 4): string {
  const n = code.size + quiet * 2;
  const dark = (x: number, y: number) => {
    const mx = x - quiet;
    const my = y - quiet;
    return mx >= 0 && my >= 0 && mx < code.size && my < code.size && code.modules[my]![mx]!;
  };
  const colour = (d: boolean) => (d ? 16 : 231);
  const lines: string[] = [];
  for (let y = 0; y < n; y += 2) {
    let line = "";
    for (let x = 0; x < n; x++) {
      line += `\x1b[38;5;${colour(dark(x, y))};48;5;${colour(dark(x, y + 1))}m▀`;
    }
    lines.push(line + "\x1b[0m");
  }
  return lines.join("\n");
}

/** A plain PBM image of the code (one pixel per module, with a quiet zone), for tests. */
export function renderQrPbm(code: QrCode, quiet = 4, scale = 4): string {
  const n = (code.size + quiet * 2) * scale;
  const rows: string[] = [];
  for (let py = 0; py < n; py++) {
    const y = Math.floor(py / scale) - quiet;
    let row = "";
    for (let px = 0; px < n; px++) {
      const x = Math.floor(px / scale) - quiet;
      const d = x >= 0 && y >= 0 && x < code.size && y < code.size && code.modules[y]![x]!;
      row += d ? "1" : "0";
    }
    rows.push(row);
  }
  return `P1\n${n} ${n}\n${rows.join("\n")}\n`;
}
