/**
 * Deriving "what changed on screen" without a terminal emulator.
 *
 * The polling fallback (used only when a control-mode stream cannot be
 * established) has no VT model, so it compares successive `capture-pane -e`
 * outputs. Three cases, in order of preference:
 *
 *   none    — nothing changed;
 *   append  — `next` extends `previous` verbatim, so the tail can be written
 *             straight out;
 *   rows    — same number of rows, so the changed rows are rewritten in place
 *             with absolute cursor addressing (`CSI <row>;1H` + `CSI K`). This
 *             is the common case for an interactive shell whose prompt line is
 *             rewritten on every keystroke, and — unlike a full repaint — it
 *             never pushes duplicate screens into the client's scrollback;
 *   repaint — anything else (different row count, scroll, resize): clear and
 *             re-send the screen and let the previous one scroll into history.
 *
 * Comparison is on decoded strings, so a multi-byte character can never be
 * split: the unit of comparison is a Unicode code point, not a byte.
 */

export type ScreenUpdateKind = "none" | "append" | "rows" | "repaint";

export interface ScreenUpdate {
  kind: ScreenUpdateKind;
  text: string;
}

export const CLEAR_AND_HOME = "\x1b[2J\x1b[H";

/** Absolute cursor addressing to the start of a 0-based row, erasing the row. */
function rewriteRow(index: number, text: string): string {
  return `\x1b[${index + 1};1H\x1b[K${text}`;
}

export function screenUpdate(previous: string, next: string): ScreenUpdate {
  if (next === previous) return { kind: "none", text: "" };
  // `startsWith` on "" is always true, so the first capture is a plain append.
  if (next.startsWith(previous)) return { kind: "append", text: next.slice(previous.length) };

  const prevRows = previous.split("\n");
  const nextRows = next.split("\n");
  if (prevRows.length !== nextRows.length) return { kind: "repaint", text: CLEAR_AND_HOME + next };

  // Longest common prefix and suffix of rows: only the middle is rewritten.
  let prefix = 0;
  while (prefix < prevRows.length && prevRows[prefix] === nextRows[prefix]) prefix++;
  let suffix = 0;
  while (
    suffix < prevRows.length - prefix &&
    prevRows[prevRows.length - 1 - suffix] === nextRows[nextRows.length - 1 - suffix]
  ) {
    suffix++;
  }

  let text = "";
  for (let i = prefix; i < nextRows.length - suffix; i++) text += rewriteRow(i, nextRows[i]);
  if (!text) return { kind: "none", text: "" };
  return { kind: "rows", text };
}