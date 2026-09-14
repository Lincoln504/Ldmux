/**
 * tmux control-mode octal decoding.
 *
 * tmux escapes any byte < 0x20 and the backslash itself as a three-digit octal
 * sequence `\NNN` in `%output` / `%extended-output` payloads. All other bytes —
 * including UTF-8 continuation/lead bytes ≥ 0x80 — pass through RAW.
 *
 * Therefore decoding MUST operate on bytes. Decoding a JS string and doing
 * `charCodeAt(i) & 0xff` corrupts any astral character (a surrogate pair gets
 * split into two low bytes). dooboxes' `byte as char` has the same class of bug.
 */

const OCTET = (c: number) => c >= 0x30 && c <= 0x37; // '0'..'7'
const BACKSLASH = 0x5c;

export function decodeOctalEscapes(input: Uint8Array): Uint8Array {
  // Fast path: no escape possible.
  if (!input.includes(BACKSLASH)) return input;
  const out: number[] = [];
  for (let i = 0; i < input.length; i++) {
    const c = input[i];
    if (c === BACKSLASH && i + 3 < input.length) {
      const a = input[i + 1];
      const b = input[i + 2];
      const d = input[i + 3];
      if (OCTET(a) && OCTET(b) && OCTET(d)) {
        out.push(((a - 0x30) << 6) | ((b - 0x30) << 3) | (d - 0x30));
        i += 3;
        continue;
      }
    }
    out.push(c);
  }
  return Uint8Array.from(out);
}