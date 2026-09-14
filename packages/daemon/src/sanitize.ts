/**
 * Stateful output sanitizer for pane bytes (ADR-0001, docs/TRANSPORT.md §5.6).
 *
 * Why this exists: pane output is replayed to a client that *answers* terminal
 * queries. An unfiltered OSC 52 lets a program write the user's clipboard, and
 * unfiltered device-status queries make xterm.js emit replies that ldmux sends
 * back into the shell (`term.onData` → `input` → `send-keys`), injecting bytes
 * that were never typed. This is the dooboxes "Patch 1" lesson.
 *
 * Policy (one instance per PaneStream, so sequences may straddle chunks):
 *   drop  OSC 52 (clipboard), all DCS/SOS/PM/APC strings, and device-status
 *         queries (DA/DSR: CSI c, CSI >c, CSI 5n, CSI 6n, CSI ?0n).
 *   keep  everything else, including SGR, cursor movement, OSC 8 hyperlinks,
 *         OSC 0/2 titles.
 */

export interface Sanitizer {
  /** Feed a chunk of decoded pane text; returns the chunk with policy applied. */
  push(text: string): string;
}

const ESC = "\x1b";
const BEL = "\x07";
const ST = ESC + "\\";
/** Guard against a malformed stream pinning an unbounded partial sequence. */
const MAX_PENDING = 8192;

type State = "ground" | "esc" | "csi" | "osc" | "oscEsc" | "string" | "stringEsc" | "overflow";

/** Device-status requests we never forward (params + final byte). */
const DROPPED_CSI_FINALS = new Set(["c", "n"]);
const DROPPED_CSI_PARAMS = new Set(["", ">", ">0", "0", "5", "6", "?0"]);

export function createSanitizer(): Sanitizer {
  let state: State = "ground";
  let pending = "";

  function reset(): void {
    state = "ground";
    pending = "";
  }

  /**
   * A malformed stream must not pin an unbounded partial sequence, and must not
   * dump the tail of it as visible text: drop until the next ESC resyncs us.
   */
  function overflow(): void {
    pending = "";
    state = "overflow";
  }

  function finishOsc(out: string): string {
    // OSC 52 = clipboard write/read. Everything else passes.
    const body = pending.slice(2);
    const num = body.split(";", 1)[0];
    if (num !== "52") out += pending;
    reset();
    return out;
  }

  return {
    push(text: string): string {
      let out = "";
      for (let i = 0; i < text.length; i++) {
        const c = text[i];
        switch (state) {
          case "ground":
            if (c === ESC) {
              state = "esc";
              pending = ESC;
            } else {
              out += c;
            }
            break;

          case "esc":
            pending += c;
            if (c === "[") state = "csi";
            else if (c === "]") state = "osc";
            else if (c === "P" || c === "X" || c === "^" || c === "_") state = "string";
            else {
              out += pending; // two-character escape (e.g. ESC 7)
              reset();
            }
            break;

          case "csi": {
            pending += c;
            const code = c.charCodeAt(0);
            if (code >= 0x40 && code <= 0x7e) {
              const final = c;
              const params = pending.slice(2, -1);
              if (!(DROPPED_CSI_FINALS.has(final) && DROPPED_CSI_PARAMS.has(params))) out += pending;
              reset();
            }
            break;
          }

          case "osc":
            if (c === BEL) {
              pending += c;
              out = finishOsc(out);
              break;
            }
            if (c === ESC) {
              pending += c;
              state = "oscEsc";
              break;
            }
            pending += c;
            break;

          case "oscEsc":
            if (c === "\\") {
              pending += c;
              out = finishOsc(out);
              break;
            }
            // Not a string terminator: keep the ESC as body content and resume.
            pending += c;
            state = "osc";
            break;

          case "string":
            if (c === BEL) reset();
            else if (c === ESC) state = "stringEsc";
            break;

          case "stringEsc":
            if (c === "\\") reset();
            else state = "string";
            break;

          case "overflow":
            if (c === ESC) {
              state = "esc";
              pending = ESC;
            }
            break;
        }
        if (pending.length > MAX_PENDING) overflow();
      }
      return out;
    },
  };
}

/** One-shot sanitization (for snapshot replays, which are standalone renders). */
export function sanitizeOnce(text: string): string {
  return createSanitizer().push(text);
}