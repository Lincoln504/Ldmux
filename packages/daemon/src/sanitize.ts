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
 *
 * OSC 52 is the one exception that can be asked for, and it is asked for in
 * `clipboardWrite` rather than assumed. Dropping it is a security decision with a
 * real cost — a program inside the shell cannot put anything on the phone's
 * clipboard, so `pi`'s copy reports success and does nothing — but honouring it
 * load-bearing: the pane is arbitrary code, and a clipboard is shared with every
 * other app on the device. A terminal that silently lets any program overwrite
 * what the user copies elsewhere is a terminal nobody asked for. So the default
 * stays closed, and the user may open it deliberately.
 *
 * The read direction has no exception at all. `GHOSTTY_TERMINAL_OPT_CLIPBOARD_READ`
 * is deliberately unset in the shim, so a program cannot ask what is on the
 * clipboard even when writes are allowed: pasting your passwords somewhere else is
 * a far worse outcome than a copy that does not arrive.
 */
export interface SanitizerOptions {
  /**
   * Pass OSC 52 through instead of dropping it. Off unless the host says
   * otherwise (see `ldmux clipboard-write`), because it widens a security
   * boundary to a program the user did not write.
   */
  clipboardWrite?: boolean;
}

export interface Sanitizer {
  /** Feed a chunk of decoded pane text; returns the chunk with policy applied. */
  push(text: string): string;
  /** Drop any partial sequence and return to the ground state (resync point). */
  reset(): void;
}

const ESC = "\x1b";
const BEL = "\x07";
const ST = ESC + "\\";
/** Guard against a malformed stream pinning an unbounded partial sequence. */
const MAX_PENDING = 8192;

type State = "ground" | "esc" | "csi" | "osc" | "oscEsc" | "string" | "stringEsc" | "overflow";

/**
 * Device queries never reach the client: a real terminal (xterm.js) *answers*
 * them, and the answer would be sent back through the pane as input.
 *
 * Keyed on the final byte alone — `c` (DA) and `n` (DSR) are queries in every
 * parameter form (`CSI c`, `CSI >c`, `CSI ?6n`, …), so an allowlist of parameter
 * strings can never be exhaustive.
 */
const DROPPED_CSI_FINALS = new Set(["c", "n"]);

export function createSanitizer(opts: SanitizerOptions = {}): Sanitizer {
  const clipboardWrite = opts.clipboardWrite === true;
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
    if (num !== "52" || clipboardWrite) out += pending;
    reset();
    return out;
  }

  return {
    reset,
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
              if (!DROPPED_CSI_FINALS.has(final)) out += pending;
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

/** One-shot sanitization (for snapshot replays, which are standalone renders).
 *
 * Snapshots never need the clipboard exception: a snapshot is a rendered capture,
 * which carries characters and no escape sequences at all. Passing the option
 * through here would be a lie about what it governs.
 */
export function sanitizeOnce(text: string): string {
  return createSanitizer().push(text);
}