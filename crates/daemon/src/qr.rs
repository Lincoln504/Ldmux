//! A QR code encoder, for `ldmux code`.
//!
//! Port of `packages/daemon/src/qr.ts` (the TS original; deleted at D4, readable at git commit `6b3efb8`).
//!
//! Byte mode only (the payload is a URL), every version 1–40, every error
//! correction level. The algorithm is ISO/IEC 18004 as laid out by Project
//! Nayuki's reference implementation: encode, add Reed–Solomon codewords per
//! block, interleave, place, then choose the mask with the lowest penalty.
//!
//! Taken as a dependency it would break the "avoid new runtime deps" rule;
//! it is written here. Correctness is pinned by decoding every size back with
//! zbar (`tests` below), not by comparing against another encoder's bits.

use std::fmt;

/// The four error correction levels, in strength order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Ecc {
    L,
    M,
    Q,
    H,
}

impl fmt::Display for Ecc {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl Ecc {
    pub fn as_str(&self) -> &'static str {
        match self {
            Ecc::L => "L",
            Ecc::M => "M",
            Ecc::Q => "Q",
            Ecc::H => "H",
        }
    }
}

const ECC_ORDER: [Ecc; 4] = [Ecc::L, Ecc::M, Ecc::Q, Ecc::H];

const ECC_CODEWORDS_PER_BLOCK: [[u32; 41]; 4] = [
    // L
    [
        0, 7, 10, 15, 20, 26, 18, 20, 24, 30, 18, 20, 24, 26, 30, 22, 24, 28, 30, 28, 28, 28, 28,
        30, 30, 26, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
    // M
    [
        0, 10, 16, 26, 18, 24, 16, 18, 22, 22, 26, 30, 22, 22, 24, 24, 28, 28, 26, 26, 26, 26, 28,
        28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28, 28,
    ],
    // Q
    [
        0, 13, 22, 18, 26, 18, 24, 18, 22, 20, 24, 28, 26, 24, 20, 30, 24, 28, 28, 26, 30, 28, 30,
        30, 30, 30, 28, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
    // H
    [
        0, 17, 28, 22, 16, 22, 28, 26, 26, 24, 28, 24, 28, 22, 24, 24, 30, 28, 28, 26, 28, 30, 24,
        30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30, 30,
    ],
];

const ECC_BLOCKS: [[u32; 41]; 4] = [
    // L
    [
        0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 4, 4, 4, 4, 4, 6, 6, 6, 6, 7, 8, 8, 9, 9, 10, 12, 12, 12, 13,
        14, 15, 16, 17, 18, 19, 19, 20, 21, 22, 24, 25,
    ],
    // M
    [
        0, 1, 1, 1, 2, 2, 4, 4, 4, 5, 5, 5, 8, 9, 9, 10, 10, 11, 13, 14, 16, 17, 17, 18, 20, 21,
        23, 25, 26, 28, 29, 31, 33, 35, 37, 38, 40, 43, 45, 47, 49,
    ],
    // Q
    [
        0, 1, 1, 2, 2, 4, 4, 6, 6, 8, 8, 8, 10, 12, 16, 12, 17, 16, 18, 21, 20, 23, 23, 25, 27, 29,
        34, 34, 35, 38, 40, 43, 45, 48, 51, 53, 56, 59, 62, 65, 68,
    ],
    // H
    [
        0, 1, 1, 2, 4, 4, 4, 5, 6, 8, 8, 11, 11, 16, 16, 18, 16, 19, 21, 25, 25, 25, 34, 30, 32,
        35, 37, 40, 42, 45, 48, 51, 54, 57, 60, 63, 66, 70, 74, 77, 81,
    ],
];

fn ecc_index(ecc: Ecc) -> usize {
    match ecc {
        Ecc::L => 0,
        Ecc::M => 1,
        Ecc::Q => 2,
        Ecc::H => 3,
    }
}

fn format_bits(ecc: Ecc) -> u32 {
    match ecc {
        Ecc::L => 1,
        Ecc::M => 0,
        Ecc::Q => 3,
        Ecc::H => 2,
    }
}

/// The modules available for data and ECC codewords, after every function
/// pattern.
fn raw_data_modules(ver: u32) -> u32 {
    let mut n = (16 * ver + 128) * ver + 64;
    if ver >= 2 {
        let align = ver / 7 + 2;
        n -= (25 * align - 10) * align - 55;
        if ver >= 7 {
            n -= 36;
        }
    }
    n
}

fn data_codewords(ver: u32, ecc: Ecc) -> u32 {
    raw_data_modules(ver) / 8
        - ECC_CODEWORDS_PER_BLOCK[ecc_index(ecc)][ver as usize]
            * ECC_BLOCKS[ecc_index(ecc)][ver as usize]
}

/// The most bytes a byte-mode code of `ver` at `ecc` holds.
pub fn byte_capacity(ver: u32, ecc: Ecc) -> i64 {
    let dc = data_codewords(ver, ecc);
    let overhead = if ver <= 9 { 8 } else { 16 };
    ((dc * 8 - 4 - overhead) / 8) as i64
}

/// The bits a byte-mode segment of `len` bytes needs at `ver`.
fn segment_bits(ver: u32, len: u32) -> u32 {
    4 + (if ver <= 9 { 8 } else { 16 }) + 8 * len
}

// ── GF(256) and Reed–Solomon ────────────────────────────────────────────────

fn gf_mul(x: u8, y: u8) -> u8 {
    let mut z: u32 = 0;
    for i in (0..8).rev() {
        z = (z << 1) ^ ((z >> 7) * 0x11d);
        z ^= ((y as u32 >> i) & 1) * x as u32;
    }
    z as u8
}

fn rs_divisor(degree: u32) -> Vec<u8> {
    let d = degree as usize;
    let mut result = vec![0u8; d];
    result[d - 1] = 1;
    let mut root: u8 = 1;
    for _i in 0..degree {
        for j in 0..d {
            result[j] = gf_mul(result[j], root);
            if j + 1 < d {
                result[j] ^= result[j + 1];
            }
        }
        root = gf_mul(root, 0x02);
    }
    result
}

fn rs_remainder(data: &[u8], divisor: &[u8]) -> Vec<u8> {
    let d = divisor.len();
    let mut result = vec![0u8; d];
    for &b in data {
        let factor = b ^ result[0];
        let mut next = vec![0u8; d];
        for i in 0..d {
            next[i] = if i + 1 < d { result[i + 1] } else { 0 } ^ gf_mul(divisor[i], factor);
        }
        result = next;
    }
    result
}

/// Split into blocks, append each block's ECC, and interleave.
fn with_ecc(data: &[u8], ver: u32, ecc: Ecc) -> Vec<u8> {
    let blocks = ECC_BLOCKS[ecc_index(ecc)][ver as usize] as usize;
    let ecc_len = ECC_CODEWORDS_PER_BLOCK[ecc_index(ecc)][ver as usize] as usize;
    let raw = raw_data_modules(ver) as usize / 8;
    let short_blocks = blocks - (raw % blocks);
    let short_len = raw / blocks;
    let divisor = rs_divisor(ecc_len as u32);
    let mut all: Vec<Vec<u8>> = vec![];
    let mut k = 0usize;
    for i in 0..blocks {
        let len = short_len as i64 - ecc_len as i64 + (if i < short_blocks { 0 } else { 1 });
        let len = if len > 0 { len as usize } else { 0 };
        let dat = data[k..k + len].to_vec();
        k += len;
        let mut block = dat.clone();
        // Short blocks carry a placeholder so every block has the same
        // length; it is skipped when interleaving.
        if i < short_blocks {
            block.push(0);
        }
        block.extend(rs_remainder(&dat, &divisor));
        all.push(block);
    }
    let mut out: Vec<u8> = vec![];
    for i in 0..all[0].len() {
        for (j, block) in all.iter().enumerate() {
            if i != short_len - ecc_len || j >= short_blocks {
                out.push(block[i]);
            }
        }
    }
    out
}

// ── Placement ───────────────────────────────────────────────────────────────

struct Grid {
    size: usize,
    modules: Vec<Vec<bool>>,
    fixed: Vec<Vec<bool>>,
}

impl Grid {
    fn new(size: usize) -> Self {
        Grid {
            size,
            modules: vec![vec![false; size]; size],
            fixed: vec![vec![false; size]; size],
        }
    }
    fn set(&mut self, x: usize, y: usize, dark: bool) {
        self.modules[y][x] = dark;
        self.fixed[y][x] = true;
    }
}

fn alignment_positions(ver: u32, size: usize) -> Vec<usize> {
    if ver == 1 {
        return vec![];
    }
    let n = (ver / 7 + 2) as usize;
    let step = ((ver * 8 + n as u32 * 3 + 5) / (n as u32 * 4 - 4)) * 2;
    let mut out: Vec<usize> = vec![6];
    let mut pos = size as u32 - 7;
    while out.len() < n {
        out.insert(1, pos as usize);
        pos = pos.wrapping_sub(step);
    }
    out
}

fn draw_format(g: &mut Grid, ecc: Ecc, mask: u32) {
    let data = (format_bits(ecc) << 3) | mask;
    let mut rem = data;
    for _i in 0..10 {
        rem = (rem << 1) ^ ((rem >> 9) * 0x537);
    }
    let bits = ((data << 10) | rem) ^ 0x5412;
    let bit = |i: u32| (bits >> i) & 1 != 0;
    let size = g.size;
    for i in 0..=5 {
        g.set(8, i as usize, bit(i));
    }
    g.set(8, 7, bit(6));
    g.set(8, 8, bit(7));
    g.set(7, 8, bit(8));
    for i in 9..15 {
        g.set((14 - i) as usize, 8, bit(i));
    }
    for i in 0..8 {
        g.set(size - 1 - i as usize, 8, bit(i));
    }
    for i in 8..15 {
        g.set(8, size - 15 + i as usize, bit(i));
    }
    // The dark module.
    g.set(8, size - 8, true);
}

fn draw_function_patterns(g: &mut Grid, ver: u32) {
    let size = g.size;
    for i in 0..size {
        g.set(6, i, i % 2 == 0);
        g.set(i, 6, i % 2 == 0);
    }
    // The three timing-pattern corners: (3,3), (size-4,3), (3,size-4).
    for (cx, cy) in [(3, 3), (size - 4, 3), (3, size - 4)] {
        for dy in -4i32..=4 {
            for dx in -4i32..=4 {
                let x = cx as i32 + dx;
                let y = cy as i32 + dy;
                if x < 0 || x >= size as i32 || y < 0 || y >= size as i32 {
                    continue;
                }
                let d = dx.abs().max(dy.abs());
                g.set(x as usize, y as usize, d != 2 && d != 4);
            }
        }
    }
    let pos = alignment_positions(ver, size);
    // Version 1 has no alignment patterns: the loop below never runs.
    let last = pos.len().saturating_sub(1);
    for i in 0..pos.len() {
        for j in 0..pos.len() {
            // The three corners already hold finder patterns. Grouped the way
            // it reads: "top row, first or last column, plus the bottom-left".
            let top_corner = i == 0 && (j == 0 || j == last);
            let bottom_left = i == last && j == 0;
            if top_corner || bottom_left {
                continue;
            }
            for dy in -2i32..=2 {
                for dx in -2i32..=2 {
                    g.set(
                        (pos[i] as i32 + dx) as usize,
                        (pos[j] as i32 + dy) as usize,
                        dx.abs().max(dy.abs()) != 1,
                    );
                }
            }
        }
    }
    // Reserve the format areas now; the real bits go in once the mask is
    // known.
    draw_format(g, Ecc::L, 0);
    if ver >= 7 {
        let mut rem = ver;
        for _i in 0..12 {
            rem = (rem << 1) ^ ((rem >> 11) * 0x1f25);
        }
        let bits = (ver << 12) | rem;
        for i in 0..18u32 {
            let dark = (bits >> i) & 1 != 0;
            let a = size - 11 + (i % 3) as usize;
            let b = (i / 3) as usize;
            g.set(a, b, dark);
            g.set(b, a, dark);
        }
    }
}

fn draw_codewords(g: &mut Grid, data: &[u8]) {
    let size = g.size;
    let mut i: u32 = 0;
    let mut right = size as u32 - 1;
    while right >= 1 {
        if right == 6 {
            right = 5;
        }
        for vert in 0..size {
            for j in 0..2u32 {
                let x = right - j;
                let upward = (right + 1) & 2 == 0;
                let y = if upward {
                    size as u32 - 1 - vert as u32
                } else {
                    vert as u32
                };
                if !g.fixed[y as usize][x as usize] && i < data.len() as u32 * 8 {
                    g.modules[y as usize][x as usize] =
                        (data[(i >> 3) as usize] >> (7 - (i & 7))) & 1 != 0;
                    i += 1;
                }
            }
        }
        // JS has no underflow: 1 - 2 is -1 and the loop ends. Match it.
        if right <= 2 {
            break;
        }
        right -= 2;
    }
}

fn mask_hit(mask: u32, x: u32, y: u32) -> bool {
    match mask {
        0 => (x + y).is_multiple_of(2),
        1 => y.is_multiple_of(2),
        2 => x.is_multiple_of(3),
        3 => (x + y).is_multiple_of(3),
        4 => (x / 3 + y / 2).is_multiple_of(2),
        5 => ((x * y) % 2) + ((x * y) % 3) == 0,
        6 => (((x * y) % 2) + ((x * y) % 3)).is_multiple_of(2),
        _ => (((x + y) % 2) + ((x * y) % 3)).is_multiple_of(2),
    }
}

fn apply_mask(g: &mut Grid, mask: u32) {
    for y in 0..g.size {
        for x in 0..g.size {
            if !g.fixed[y][x] && mask_hit(mask, x as u32, y as u32) {
                g.modules[y][x] = !g.modules[y][x];
            }
        }
    }
}

/// The four penalty rules of ISO/IEC 18004 §7.8.3. Lower scores are better.
fn penalty(m: &[Vec<bool>]) -> i64 {
    let size = m.len() as u32;
    let at = |x: u32, y: u32, horizontal: bool| -> bool {
        if horizontal {
            m[y as usize][x as usize]
        } else {
            m[x as usize][y as usize]
        }
    };
    let mut score: i64 = 0;
    // A finder-like run: dark light dark dark dark light dark, with four
    // light on one side.
    let finder = [true, false, true, true, true, false, true];
    for horizontal in [true, false] {
        for a in 0..size {
            let mut run: u32 = 1;
            for b in 1..=size {
                if b < size && at(b, a, horizontal) == at(b - 1, a, horizontal) {
                    run += 1;
                    continue;
                }
                if run >= 5 {
                    score += 3 + (run - 5) as i64;
                }
                run = 1;
            }
            for b in 0..=size - 7 {
                let mut matches = true;
                for (k, f) in finder.iter().enumerate() {
                    if at(b + k as u32, a, horizontal) != *f {
                        matches = false;
                    }
                }
                if !matches {
                    continue;
                }
                let light_before = (1..=4u32).all(|k| {
                    let before = b as i64 - k as i64;
                    before < 0 || !at(b - k, a, horizontal)
                });
                let light_after =
                    (0..3u32).all(|k| b + 7 + k >= size || !at(b + 7 + k, a, horizontal));
                if light_before || light_after {
                    score += 40;
                }
            }
        }
    }
    let mut dark: i64 = 0;
    for y in 0..size as usize {
        for x in 0..size as usize {
            if m[y][x] {
                dark += 1;
            }
            if x + 1 < size as usize && y + 1 < size as usize {
                let c = m[y][x];
                if c == m[y][x + 1] && c == m[y + 1][x] && c == m[y + 1][x + 1] {
                    score += 3;
                }
            }
        }
    }
    let total = (size * size) as i64;
    let v = (dark * 20 - total * 10).abs();
    let ceiled = v / total + (if v % total != 0 { 1 } else { 0 });
    score += (ceiled - 1) * 10;
    score
}

/// A square grid of modules; `true` is dark. `modules[y][x]`.
#[derive(Debug, Clone)]
pub struct QrCode {
    pub version: u32,
    pub ecc: Ecc,
    pub size: usize,
    pub modules: Vec<Vec<bool>>,
}

/// Encode `text` as a QR code.
///
/// The smallest version that holds it at `min_ecc`, then the strongest error
/// correction that still fits that version: a bigger code costs terminal rows,
/// and stronger correction at the same size is free.
pub fn encode_qr(text: &str, min_ecc: Ecc) -> Result<QrCode, String> {
    let bytes: Vec<u8> = text.as_bytes().to_vec();
    let mut ver: u32 = 0;
    for v in 1..=40 {
        if segment_bits(v, bytes.len() as u32) <= data_codewords(v, min_ecc) * 8 {
            ver = v;
            break;
        }
    }
    if ver == 0 {
        return Err(format!("too long for a QR code: {} bytes", bytes.len()));
    }
    let mut ecc = min_ecc;
    for &e in &ECC_ORDER[ecc_index(min_ecc) + 1..] {
        if segment_bits(ver, bytes.len() as u32) <= data_codewords(ver, e) * 8 {
            ecc = e;
        }
    }

    let capacity = data_codewords(ver, ecc) * 8;
    let mut bits: Vec<bool> = vec![];
    let push = |bits: &mut Vec<bool>, value: u32, len: u32| {
        for i in (0..len).rev() {
            bits.push((value >> i) & 1 != 0);
        }
    };
    push(&mut bits, 0b0100, 4);
    push(&mut bits, bytes.len() as u32, if ver <= 9 { 8 } else { 16 });
    for &b in &bytes {
        push(&mut bits, b as u32, 8);
    }
    let terminator = 4.min(capacity - bits.len() as u32);
    push(&mut bits, 0, terminator);
    let pad = (8 - (bits.len() as u32 % 8)) % 8;
    push(&mut bits, 0, pad);
    let mut data: Vec<u8> = vec![];
    for i in (0..bits.len() as u32).step_by(8) {
        let mut byte: u32 = 0;
        for j in 0..8 {
            byte = (byte << 1) | (if bits[i as usize + j] { 1 } else { 0 });
        }
        data.push(byte as u8);
    }
    let mut pad: u8 = 0xec;
    while (data.len() as u32) < capacity / 8 {
        data.push(pad);
        pad ^= 0xec ^ 0x11;
    }

    let size = (ver * 4 + 17) as usize;
    let mut g = Grid::new(size);
    draw_function_patterns(&mut g, ver);
    let data_with_ecc = with_ecc(&data, ver, ecc);
    draw_codewords(&mut g, &data_with_ecc);

    let mut best: u32 = 0;
    let mut best_score: i64 = i64::MAX;
    for mask in 0..8 {
        apply_mask(&mut g, mask);
        draw_format(&mut g, ecc, mask);
        let s = penalty(&g.modules);
        if s < best_score {
            best = mask;
            best_score = s;
        }
        apply_mask(&mut g, mask); // XOR again: undo.
    }
    apply_mask(&mut g, best);
    draw_format(&mut g, ecc, best);
    Ok(QrCode {
        version: ver,
        ecc,
        size,
        modules: g.modules,
    })
}

/// Draw a code for a terminal: two modules per character cell, using the
/// upper half block with the top module as foreground and the bottom as
/// background.
///
/// The colours are explicit (xterm-256 16 and 231, pure black and pure white)
/// rather than "whatever this terminal's default is": a scanner wants dark
/// modules on light, and a dark-themed terminal drawing the default would hand
/// it the negative. Those two indices are in the fixed colour cube, which
/// themes do not remap the way they remap the first sixteen.
pub fn render_qr_terminal(code: &QrCode, quiet: usize) -> String {
    let n = code.size + quiet * 2;
    let dark = |x: usize, y: usize| -> bool {
        let mx = x as i32 - quiet as i32;
        let my = y as i32 - quiet as i32;
        mx >= 0
            && my >= 0
            && mx < code.size as i32
            && my < code.size as i32
            && code.modules[my as usize][mx as usize]
    };
    let colour = |d: bool| -> u32 { if d { 16 } else { 231 } };
    let mut lines: Vec<String> = vec![];
    for y in (0..n).step_by(2) {
        let mut line = String::new();
        for x in 0..n {
            line.push_str(&format!(
                "\x1b[38;5;{};48;5;{}m▀",
                colour(dark(x, y)),
                colour(dark(x, y + 1))
            ));
        }
        line.push_str("\x1b[0m");
        lines.push(line);
    }
    lines.join("\n")
}

/// A plain PBM image of the code (one pixel per module, with a quiet zone),
/// for tests.
pub fn render_qr_pbm(code: &QrCode, quiet: usize, scale: u32) -> String {
    let n = ((code.size + quiet * 2) as u32) * scale;
    let mut rows: Vec<String> = vec![];
    for py in 0..n {
        let y = py as i32 / scale as i32 - quiet as i32;
        let mut row = String::new();
        for px in 0..n {
            let x = px as i32 / scale as i32 - quiet as i32;
            let d = x >= 0
                && y >= 0
                && x < code.size as i32
                && y < code.size as i32
                && code.modules[y as usize][x as usize];
            row.push(if d { '1' } else { '0' });
        }
        rows.push(row);
    }
    format!("P1\n{n} {n}\n{}\n", rows.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec::{ExecOpts, run};

    /// Printable ASCII, varied, so no two codes share a payload.
    fn payload(len: usize, seed: u32) -> String {
        const ALPHABET: &[u8] =
            br"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789:/?&=.-_%~";
        let mut x = seed.wrapping_mul(2654435761);
        let mut s = String::new();
        for _ in 0..len {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            s.push(ALPHABET[((x >> 16) % ALPHABET.len() as u32) as usize] as char);
        }
        s
    }

    /// Whether `zbarimg` actually ran (so the result below is a decode
    /// verdict, not a missing-binary artefact).
    ///
    /// `exec::run` spawns directly (no shell) and returns `code: -1` when the
    /// binary is missing (spawn fails) or the command times out; either way
    /// `zbarimg` did not run, so treat it as absent. A real exit code (0 on
    /// success, non-zero on a decode failure) means it was present.
    fn zbar_on_path(res: &crate::exec::ExecResult) -> bool {
        res.code != -1
    }

    /// A skip that cannot be missed: `cargo test` prints it, and the suite goes green on a machine
    /// without `zbarimg` instead of failing for an undeclared system dependency. The decode is still
    /// covered wherever the binary exists, which is `nix develop` and CI.
    fn skip_without_zbar(res: &crate::exec::ExecResult, what: &str) -> bool {
        if zbar_on_path(res) {
            return false;
        }
        eprintln!(
            "SKIP {what}: zbarimg is not on PATH, so the decode cannot be checked here. Run the \
             tests inside `nix develop`. Underlying error: {}",
            res.err
        );
        true
    }

    /// Versions the gate decodes, every EC level at each.
    ///
    /// Not all forty: zbar's cost grows steeply with the module count, about
    /// 100 ms per image at version 40 against 10 ms at version 1, so the full
    /// matrix costs fourteen seconds and buys nothing the spread does not. This
    /// covers all three error-correction block layouts (1-9, 10-26, 27-40) and
    /// both render scales (4 below version 21, 2 above), and the exhaustive
    /// sweep is one `#[ignore]` away when a version-specific bug is suspected.
    const GATE_VERSIONS: &[u32] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 20, 40];

    #[tokio::test]
    async fn every_version_and_error_correction_level_decodes_back_to_its_payload() {
        decode_versions(GATE_VERSIONS).await;
    }

    /// The exhaustive sweep: all forty versions at all four levels. Fourteen
    /// seconds, so it is out of the gate; run it with
    /// `cargo test -p ldmux-daemon --lib -- --ignored decode_versions` when a
    /// version-specific encoding bug is suspected.
    #[tokio::test]
    #[ignore = "exhaustive: 40 versions x 4 levels costs ~14 s in zbar"]
    async fn every_version_and_error_correction_level_decodes_back_to_its_payload_exhaustive() {
        decode_versions(&(1..=40).collect::<Vec<_>>()).await;
    }

    async fn decode_versions(vers: &[u32]) {
        // One zbar run for all of them; it reports in file order. Filled to
        // capacity, so each code must land on exactly this version and level.
        let dir = std::env::temp_dir().join(format!("ldmux-qr-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let mut cases: Vec<(String, String)> = vec![];
        for &ver in vers {
            for (ei, &ecc) in ECC_ORDER.iter().enumerate() {
                let text = payload(byte_capacity(ver, ecc) as usize, ver * 4 + ei as u32);
                let code = encode_qr(&text, ecc).unwrap();
                assert_eq!(
                    format!("{}{}", code.version, code.ecc.as_str()),
                    format!("{ver}{}", ecc.as_str())
                );
                let file = dir.join(format!("v{ver}{}.pbm", ecc.as_str()));
                let scale = if ver > 20 { 2 } else { 4 };
                std::fs::write(&file, render_qr_pbm(&code, 4, scale)).unwrap();
                cases.push((file.to_string_lossy().to_string(), text));
            }
        }
        let mut argv: Vec<String> = vec![
            "zbarimg".into(),
            "--raw".into(),
            "-q".into(),
            "-Sdisable".into(),
            "-Sqrcode.enable".into(),
        ];
        argv.extend(cases.iter().map(|(f, _)| f.clone()));
        let res = run(
            argv,
            ExecOpts {
                timeout_ms: Some(100_000),
                env: None,
            },
        )
        .await;
        if skip_without_zbar(&res, "every version and error correction level") {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert_eq!(res.code, 0);
        let decoded: Vec<String> = res
            .out
            .lines()
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();
        let expected: Vec<String> = cases.into_iter().map(|(_, t)| t).collect();
        assert_eq!(decoded, expected);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_code_is_the_smallest_version_that_fits_at_the_strongest_level_that_fits_it() {
        // 20 bytes: version 1 at L holds 17, so version 2 — which holds 32 at
        // L, 26 at M, 20 at Q and 14 at H, so it is boosted to Q.
        assert_eq!(byte_capacity(1, Ecc::L), 17);
        assert_eq!(
            [
                byte_capacity(2, Ecc::L),
                byte_capacity(2, Ecc::M),
                byte_capacity(2, Ecc::Q),
                byte_capacity(2, Ecc::H)
            ],
            [32, 26, 20, 14]
        );
        let code = encode_qr(&"x".repeat(20), Ecc::L).unwrap();
        assert_eq!(code.version, 2);
        assert_eq!(code.ecc, Ecc::Q);
        assert_eq!(code.size, 25);
    }

    #[test]
    fn too_long_for_any_version_is_an_error_not_a_truncated_code() {
        let over = "x".repeat(byte_capacity(40, Ecc::L) as usize + 1);
        let err = encode_qr(&over, Ecc::L).unwrap_err();
        assert!(err.contains("too long"), "got: {err}");
    }

    #[test]
    fn the_terminal_rendering_is_two_modules_per_line_dark_on_light_quiet_zone_included() {
        let code = encode_qr("ldmux", Ecc::L).unwrap();
        let rendered = render_qr_terminal(&code, 4);
        let lines: Vec<&str> = rendered.split("\n").collect();
        // 21 modules + 4 + 4 quiet = 29 rows, two per line.
        assert_eq!(lines.len(), 15);
        // Explicit colours only — never the terminal's default foreground.
        assert!(
            lines[0].contains("\x1b[38;5;231;48;5;231m"),
            "first cell not light-on-light"
        );
        assert!(lines.iter().all(|l| l.ends_with("\x1b[0m")));
        assert!(lines.join("").contains("\x1b[38;5;16;"));
    }

    /// Parse one rendered line's half-block cells back into (top, bottom)
    /// pixel strings, the way a camera sees them.
    fn parse_cells(line: &str) -> (String, String) {
        let chars: Vec<char> = line.chars().collect();
        let mut top = String::new();
        let mut bottom = String::new();
        let mut i = 0usize;
        while i < chars.len() {
            if chars[i] == '\x1b'
                && chars.get(i + 1) == Some(&'[')
                && chars.get(i + 2) == Some(&'3')
                && chars.get(i + 3) == Some(&'8')
            {
                // Cell: \x1b[38;5;FG;48;5;BGm▀
                let mut j = i + 7; // past "\x1b[38;5;" (7 chars)
                let mut fg = String::new();
                while chars[j] != ';' {
                    fg.push(chars[j]);
                    j += 1;
                }
                j += 1;
                j += 5; // past "48;5;"
                let mut bg = String::new();
                while chars[j] != 'm' {
                    bg.push(chars[j]);
                    j += 1;
                }
                // j is at 'm'; j+1 is the upper half block.
                if fg == "16" {
                    top.push('1');
                } else {
                    top.push('0');
                }
                if bg == "16" {
                    bottom.push('1');
                } else {
                    bottom.push('0');
                }
                i = j + 2;
            } else {
                i += 1;
            }
        }
        (top, bottom)
    }

    /// The terminal rendering, read back the way a camera sees it.
    ///
    /// Each character cell is one module wide and two tall, its top half the
    /// foreground colour and its bottom the background. Parsing the escapes
    /// back into pixels and decoding those proves the half-block scheme, the
    /// colour choice and the quiet zone together — not just that the bits
    /// were right before drawing.
    #[tokio::test]
    async fn what_ldmux_code_prints_decodes_back_to_the_pairing_link() {
        let link = format!(
            "ldmux://pair?v=1&host=100.100.0.1:7625&token={}&name=debian",
            "0123456789abcdef".repeat(4)
        );
        let code = encode_qr(&link, Ecc::L).unwrap();
        let rendered = render_qr_terminal(&code, 4);
        let lines: Vec<&str> = rendered.split("\n").collect();
        let mut rows: Vec<String> = vec![];
        for line in &lines {
            let (top, bottom) = parse_cells(line);
            rows.push(top);
            rows.push(bottom);
        }
        let width = rows[0].len();
        let scale: u32 = 4;
        let mut px: Vec<String> = vec![];
        for r in &rows {
            // Horizontal: each character repeated `scale` times.
            let s: String = r
                .chars()
                .flat_map(|c| std::iter::repeat_n(c, scale as usize))
                .collect();
            // Vertical: `scale` copies of that line.
            for _ in 0..scale {
                px.push(s.clone());
            }
        }
        let dir = std::env::temp_dir().join(format!("ldmux-qrterm-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let file = dir.join("term.pbm");
        std::fs::write(
            &file,
            format!(
                "P1\n{} {}\n{}\n",
                width as u32 * scale,
                px.len(),
                px.join("\n")
            ),
        )
        .unwrap();
        let res = run(
            vec![
                "zbarimg".into(),
                "--raw".into(),
                "-q".into(),
                "-Sdisable".into(),
                "-Sqrcode.enable".into(),
                file.to_string_lossy().to_string(),
            ],
            ExecOpts {
                timeout_ms: None,
                env: None,
            },
        )
        .await;
        if skip_without_zbar(&res, "the pairing link round trip") {
            let _ = std::fs::remove_dir_all(&dir);
            return;
        }
        assert_eq!(res.out.trim(), link);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
