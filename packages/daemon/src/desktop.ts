/**
 * Desktop mirror (ADR/SPEC §desktop mirror): open a *read-only by default*
 * terminal view of an ldmux session on the machine's own screen.
 *
 * Read-only matters: input is owned by the phone, and a second writable client
 * would produce split input (`tmux attach -r` is the read-only client).
 *
 * The command construction is pure and unit-tested; the side effect (i3-msg +
 * alacritty) is isolated in `openMirror` so it can be skipped headlessly.
 */

import { run } from "./exec.ts";

export interface MirrorOptions {
  /** Read-only client (default true): mirror, never steal input. */
  readOnly?: boolean;
  /** i3 workspace number to place the mirror on (default 10, the last one). */
  workspace?: number;
  /** Terminal emulator binary. */
  terminal?: string;
  /** X window class, so i3 can target/float it. */
  windowClass?: string;
}

export const DEFAULT_MIRROR_WORKSPACE = 10;
export const DEFAULT_MIRROR_CLASS = "ldmux-mirror";

/** tmux argv for a mirror client (no emulator wrapper). */
export function mirrorTmuxArgs(tmuxSocket: string, tmuxName: string, opts: MirrorOptions = {}): string[] {
  const ro = opts.readOnly ?? true;
  return ["-L", tmuxSocket, "attach", ...(ro ? ["-r"] : []), "-t", tmuxName];
}

/** Full shell-less argv: `<terminal> --class <class> -e tmux <args...>`. */
export function mirrorArgv(tmuxSocket: string, tmuxName: string, opts: MirrorOptions = {}): string[] {
  const terminal = opts.terminal ?? "alacritty";
  const windowClass = opts.windowClass ?? DEFAULT_MIRROR_CLASS;
  return [terminal, "--class", windowClass, "-e", "tmux", ...mirrorTmuxArgs(tmuxSocket, tmuxName, opts)];
}

/**
 * Quote an argument for a POSIX shell. Space-containing arguments were already
 * quoted; this also neutralises space-free metacharacters (`$(…)`, backticks,
 * `;`, `|`), which matter because `mirrorCommand` is passed to `i3-msg exec`,
 * which runs its argument through a shell.
 */
function shellQuote(a: string): string {
  if (/^[A-Za-z0-9_@%+=:,./-]+$/.test(a)) return a;
  return `'${a.replaceAll("'", `'\\''`)}'`;
}

/** Human/machine readable command line (what the CLI prints). */
export function mirrorCommand(tmuxSocket: string, tmuxName: string, opts: MirrorOptions = {}): string {
  return mirrorArgv(tmuxSocket, tmuxName, opts).map(shellQuote).join(" ");
}

export interface MirrorResult {
  ok: boolean;
  workspace: number;
  argv: string[];
  detail?: string;
}

/**
 * Open the mirror via i3 (X11). Returns `ok: false` with a detail message when
 * i3 is unavailable — the caller treats this as best-effort, never fatal.
 */
export async function openMirror(
  tmuxSocket: string,
  tmuxName: string,
  opts: MirrorOptions = {},
): Promise<MirrorResult> {
  const workspace = opts.workspace ?? DEFAULT_MIRROR_WORKSPACE;
  const argv = mirrorArgv(tmuxSocket, tmuxName, opts);
  if (!process.env.DISPLAY) return { ok: false, workspace, argv, detail: "no DISPLAY (not an X11 session)" };
  const i3 = await run(["i3-msg", `workspace number ${workspace}`]);
  if (i3.code !== 0) {
    return { ok: false, workspace, argv, detail: "i3-msg not available" };
  }
  const exec = await run(["i3-msg", `exec -- ${mirrorCommand(tmuxSocket, tmuxName, opts)}`]);
  if (exec.code !== 0) {
    return { ok: false, workspace, argv, detail: "i3 exec failed" };
  }
  return { ok: true, workspace, argv };
}