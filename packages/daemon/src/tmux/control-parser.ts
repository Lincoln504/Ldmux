/**
 * Parser for the tmux control-mode (`tmux -C`) notification stream.
 *
 * Pure and testable: `feed()` consumes raw stdout bytes and invokes handlers.
 * It handles the correctness requirements identified in docs/TRANSPORT.md:
 *
 *   - line framing on LF only (raw payloads never contain LF; it is escaped as
 *     \012) — do NOT use Node readline (it splits on U+2028/9);
 *   - octal -> BYTES at the byte level, then a per-pane incremental UTF-8
 *     decoder so a multibyte character split across notifications is reassembled
 *     (and so raw UTF-8 payloads are not corrupted);
 *   - `%begin/%end/%error <ts> <seq> <flags>` correlation: the sequence number
 *     is reported so the client can match a response to its own command (with
 *     an ordered fallback for tmux builds that do not number responses);
 *   - `%output` and `%extended-output` (flow control) plus `%pause`/`%continue`;
 *   - unknown `%...` notifications are forwarded for forward-compatibility.
 */

import { decodeOctalEscapes } from "./octal.ts";

const LF = 0x0a;
const SP = 0x20;
const COLON = 0x3a;

export interface ControlParserHandlers {
  /** Decoded terminal text for a pane (UTF-8 reassembled across records). */
  onOutput?: (pane: string, text: string) => void;
  /** A `%...` notification, or a stray non-block line. */
  onNotification?: (line: string) => void;
  /** A command response block closed by `%end` (ok) or `%error`. */
  onResponse?: (seq: number, ok: boolean, lines: string[]) => void;
  /** tmux paused a pane because we fell behind (`%pause %<pane>`). */
  onPause?: (pane: string) => void;
  /** tmux resumed a pane (`%continue %<pane>`). */
  onContinue?: (pane: string) => void;
  onExit?: () => void;
}

export class ControlParser {
  private buf: Uint8Array = new Uint8Array(0);
  private readonly paneDecoders = new Map<string, TextDecoder>();
  private readonly text = new TextDecoder("utf-8", { fatal: false });
  private inBlock = false;
  private blockLines: string[] = [];

  constructor(private readonly handlers: ControlParserHandlers) {}

  feed(chunk: Uint8Array): void {
    if (chunk.length === 0) return;
    if (this.buf.length === 0) {
      this.buf = chunk;
    } else {
      const merged = new Uint8Array(this.buf.length + chunk.length);
      merged.set(this.buf, 0);
      merged.set(chunk, this.buf.length);
      this.buf = merged;
    }
    let start = 0;
    for (let i = 0; i < this.buf.length; i++) {
      if (this.buf[i] === LF) {
        this.handleLine(this.buf.subarray(start, i));
        start = i + 1;
      }
    }
    this.buf = this.buf.subarray(start);
  }

  private paneDecoder(pane: string): TextDecoder {
    let d = this.paneDecoders.get(pane);
    if (!d) {
      d = new TextDecoder("utf-8", { fatal: false });
      this.paneDecoders.set(pane, d);
    }
    return d;
  }

  private handleLine(line: Uint8Array): void {
    if (startsWith(line, "%begin ")) {
      this.inBlock = true;
      this.blockLines = [];
      return;
    }
    if (startsWith(line, "%end ") || startsWith(line, "%error ")) {
      this.inBlock = false;
      const ok = startsWith(line, "%end ");
      this.handlers.onResponse?.(this.sequence(line), ok, this.blockLines);
      return;
    }
    if (this.inBlock) {
      this.blockLines.push(this.text.decode(line));
      return;
    }
    if (startsWith(line, "%output ")) {
      const rest = line.subarray(8);
      const sp = rest.indexOf(SP);
      if (sp > 0) {
        const pane = this.text.decode(rest.subarray(0, sp));
        this.emitOutput(pane, rest.subarray(sp + 1));
      }
      return;
    }
    if (startsWith(line, "%extended-output ")) {
      // %extended-output %<pane> <age_ms> : <data>
      const rest = line.subarray(17);
      const sp = rest.indexOf(SP);
      if (sp <= 0) return;
      const pane = this.text.decode(rest.subarray(0, sp));
      const colon = indexOfTriple(rest, SP, COLON, SP, sp);
      this.emitOutput(pane, colon >= 0 ? rest.subarray(colon + 3) : new Uint8Array(0));
      return;
    }
    if (startsWith(line, "%pause ")) {
      this.handlers.onPause?.(this.paneId(line, 7));
      return;
    }
    if (startsWith(line, "%continue ")) {
      this.handlers.onContinue?.(this.paneId(line, 10));
      return;
    }
    if (startsWith(line, "%exit")) {
      this.handlers.onExit?.();
      return;
    }
    const s = this.text.decode(line);
    if (s.length > 0) this.handlers.onNotification?.(s);
  }

  private emitOutput(pane: string, payload: Uint8Array): void {
    const bytes = decodeOctalEscapes(payload);
    const text = this.paneDecoder(pane).decode(bytes, { stream: true });
    if (text.length > 0) this.handlers.onOutput?.(pane, text);
  }

  private sequence(line: Uint8Array): number {
    const n = Number(this.text.decode(line).split(" ")[2]);
    return Number.isFinite(n) ? n : -1;
  }

  /** The `%<pane>` argument of a `%pause`/`%continue` notification. */
  private paneId(line: Uint8Array, from: number): string {
    return this.text.decode(line.subarray(from)).trim();
  }
}

function startsWith(line: Uint8Array, prefix: string): boolean {
  if (line.length < prefix.length) return false;
  for (let i = 0; i < prefix.length; i++) {
    if (line[i] !== prefix.charCodeAt(i)) return false;
  }
  return true;
}

function indexOfTriple(line: Uint8Array, a: number, b: number, c: number, from: number): number {
  for (let i = from; i + 2 < line.length; i++) {
    if (line[i] === a && line[i + 1] === b && line[i + 2] === c) return i;
  }
  return -1;
}