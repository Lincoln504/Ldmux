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
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";

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

/**
 * Shells that cannot use a mouse click.
 *
 * The mouse mode is sticky: a program that enables reporting and then exits
 * leaves the mode enabled in both the emulator and tmux, so `mouse`/`mouseSgr`
 * still read true while a shell is what would receive the bytes. A shell prints
 * them — the `^[[<0;42;8M` garbage that appeared on a phone tap — so the
 * synthesized click has to be refused before it reaches the pty.
 */
const SHELLS = new Set([
  "bash", "sh", "dash", "ash", "zsh", "fish", "ksh", "ksh93", "mksh",
  "tcsh", "csh", "yash", "nu", "nushell", "elvish", "xonsh", "pwsh",
]);

/** Is this pane's foreground command an interactive shell? */
export function isShellForeground(command: string): boolean {
  return SHELLS.has(command.trim().toLowerCase().replace(/^-/, ""));
}

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

/**
 * The i3 window class for a session's live window.
 *
 * Derived from the session id, and sanitised: the class is interpolated into an
 * i3 criterion (`[class="…"]`), so anything outside `[A-Za-z0-9_-]` has to go —
 * an id from `adopt` is caller-supplied, and a quote in it would end the criterion
 * and turn the rest into i3 commands.
 */
export function liveWindowClass(sessionId: string): string {
  const safe = sessionId.replace(/[^A-Za-z0-9_-]/g, "").slice(0, 40);
  return `ldmux-live-${safe}`;
}

/**
 * An i3 workspace target, or null when the name could be i3 syntax.
 *
 * `move to workspace number <n>` is argv to i3, but the name also reaches the
 * window manager as a criterion in `closeLive`, so it is restricted to the
 * characters real workspace names use.
 */
export function safeWorkspace(name: string): string | null {
  const trimmed = name.trim();
  return /^[A-Za-z0-9._-]{1,32}$/.test(trimmed) ? trimmed : null;
}

/**
 * argv for a session's live window.
 *
 * It runs the shim (see [liveShimScript]) rather than tmux directly, because a
 * live window has to survive being detached to say who took the shell. Sheel,
 * not node: this runs in whatever environment the window manager hands a new
 * window, and `sh` is the one thing guaranteed to be there.
 */
export function liveArgv(
  shimPath: string,
  sessionId: string,
  terminal = "alacritty",
): string[] {
  return [terminal, "--class", liveWindowClass(sessionId), "-e", "/bin/sh", shimPath];
}

export interface LiveSpec {
  /** The generated shim's path. */
  shimPath: string;
  argv: string[];
}

/**
 * Write a session's live-window shim and return what to run.
 *
 * Written with mode 700 and overwritten on every make-live: it embeds the tmux
 * socket and session name, so a stale copy is a window that attaches to the wrong
 * thing, and a world-writable one is a command somebody else can replace.
 */
export async function prepareLive(
  stateDir: string,
  tmuxSocket: string,
  tmuxName: string,
  sessionId: string,
  tmuxBin: string,
  terminal?: string,
): Promise<LiveSpec> {
  const dir = join(stateDir, "live");
  await mkdir(dir, { recursive: true, mode: 0o700 });
  const shimPath = join(dir, `live-${liveWindowClass(sessionId)}.sh`);
  await writeFile(shimPath, liveShimScript(tmuxBin, tmuxSocket, tmuxName), { mode: 0o700 });
  return { shimPath, argv: liveArgv(shimPath, sessionId, terminal) };
}

/**
 * The live window's shim, as a shell script.
 *
 * A live window cannot be a bare `tmux attach` any more, and the reason is the
 * whole of ADR-0010: under exclusive ownership a window is *detached* when
 * another device takes the shell over, and a bare attach would exit on that —
 * the window would vanish instead of saying who has it. So the window runs a
 * loop: attach, and when the attach returns, say what happened and wait.
 *
 * The reclaim is the `read`: pressing a key re-attaches, and the daemon sees the
 * window's client appear while another device holds the shell and takes it back.
 * That keystroke *is* the consent — nothing reclaims silently — which is why the
 * shim needs no protocol of its own and never talks to the daemon.
 *
 * The tty is recorded in a tmux session option before attaching, because that is
 * the only handle on a client the window itself can write down, and it is how the
 * daemon tells "this window is still here" from "this window was closed" — the
 * difference between a shell being taken away and a shell being disposed of.
 */
export function liveShimScript(tmuxBin: string, tmuxSocket: string, tmuxName: string): string {
  // The tmux invocation prefix, with every part quoted: a socket or session name
  // can contain shell metacharacters, and this text is executed.
  const t = `${shellQuote(tmuxBin)} -L ${shellQuote(tmuxSocket)}`;
  const name = shellQuote(tmuxName);
  return `#!/bin/sh
# Generated by ldmuxd for one live window. Do not edit: it is rewritten on every
# make-live, and a stale copy is a window that talks to the wrong session.
#
# Record which client this window is. The daemon compares it against tmux's
# client list, which is how it tells this window still being here (we detached
# it) from this window being closed (the user is done with the shell).
LDMUX_LIVE_TTY=$(tty 2>/dev/null || true)
if [ -n "$LDMUX_LIVE_TTY" ]; then
  ${t} set-option -t ${name} -q @ldmux_live_tty "$LDMUX_LIVE_TTY"
fi
while :; do
  ${t} attach -t ${name}
  if ! ${t} has-session -t ${name} 2>/dev/null; then
    printf '\\n  This shell has ended.\\n\\n'
    break
  fi
  printf '\\n\\n  LDMUX\\n\\n  This shell is open on another device.\\n  Press Enter to take it back.\\n\\n'
  # One keypress is the consent. \`read\` also fails when the window is closed
  # while we wait, which is exactly when looping again would be wrong.
  read -r _ || break
done
`;
}

export interface MirrorResult {
  ok: boolean;
  workspace: number;
  argv: string[];
  detail?: string;
}

/**
 * Move a window to a workspace **without following it**.
 *
 * The obvious way to do this — `i3-msg 'workspace N; exec …'` — switches the
 * user's view to workspace N, which is the opposite of what someone tapping a
 * button on their phone wants: their desktop jumps under them. Instead the window
 * is launched where it is, waited for, and *moved*. Only the window moves.
 *
 * Returns the window's id once i3 reports it, or null if it never appeared.
 */
export async function placeLive(
  sessionId: string,
  workspace: string,
  opts: { timeoutMs?: number } = {},
): Promise<{ ok: boolean; windowId?: string; detail?: string }> {
  const target = safeWorkspace(workspace);
  if (target === null) return { ok: false, detail: "invalid workspace name" };
  if (!process.env.DISPLAY) return { ok: false, detail: "no DISPLAY (not an X11 session)" };

  const className = liveWindowClass(sessionId);
  const deadline = Date.now() + (opts.timeoutMs ?? 5000);

  // Poll for the window rather than sleeping a guessed interval: a terminal can
  // take a moment to map, and the move is a no-op until it exists.
  while (Date.now() < deadline) {
    const tree = await run(["i3-msg", "-t", "get_tree"], { timeoutMs: 3000 });
    if (tree.code !== 0) {
      // i3 is gone, or was never the window manager here.
      if (Date.now() > deadline - 4000) return { ok: false, detail: "i3-msg not available" };
    } else {
      const windowId = findWindowIdByClass(tree.out, className);
      if (windowId) {
        const moved = await run(
          ["i3-msg", `[id="${windowId}"] move to workspace number ${target}`],
          { timeoutMs: 3000 },
        );
        if (moved.code !== 0) return { ok: false, windowId, detail: "i3 move failed" };
        return { ok: true, windowId };
      }
    }
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  return { ok: false, detail: "the window did not appear" };
}

/**
 * Is a session's live window still on screen?
 *
 * Three answers, not two. "Unknown" is not "absent": the caller uses this to
 * decide whether a shell was *disposed of*, and treating "i3 is not answering" as
 * "the window was closed" would kill sessions every time the window manager
 * hiccupped. A success reply is a parse acknowledgement, not evidence.
 */
export async function liveWindowState(
  sessionId: string,
): Promise<"present" | "absent" | "unknown"> {
  if (!process.env.DISPLAY) return "unknown";
  const res = await run(["i3-msg", "-t", "get_tree"], { timeoutMs: 3000 });
  if (res.code !== 0) return "unknown";
  return findWindowIdByClass(res.out, liveWindowClass(sessionId)) !== null ? "present" : "absent";
}

/** Close a session's live window, if it is open. */
export async function closeLive(sessionId: string): Promise<{ ok: boolean; detail?: string }> {
  if (!process.env.DISPLAY) return { ok: false, detail: "no DISPLAY (not an X11 session)" };
  const res = await run(["i3-msg", `[class="${liveWindowClass(sessionId)}"] kill`], { timeoutMs: 3000 });
  // i3 answers an unmatched criterion with success too, so this says "asked",
  // not "was open" — which is the honest thing to report.
  return res.code === 0 ? { ok: true } : { ok: false, detail: "i3-msg not available" };
}

/**
 * Find a window id in an i3 tree by window class.
 *
 * Walks the whole tree because a class match is the only reliable criterion: the
 * window may be floating, or nested in a split, by the time it is found.
 */
export function findWindowIdByClass(treeJson: string, className: string): string | null {
  let root: unknown;
  try {
    root = JSON.parse(treeJson);
  } catch {
    return null;
  }
  const visit = (node: unknown): string | null => {
    if (!node || typeof node !== "object") return null;
    const n = node as {
      window?: number | null;
      app_id?: string | null;
      window_properties?: { class?: string } | null;
      nodes?: unknown[];
      floating_nodes?: unknown[];
    };
    if (typeof n.window === "number" && n.window !== 0) {
      const cls = n.app_id ?? n.window_properties?.class ?? "";
      if (cls === className) return `0x${n.window.toString(16)}`;
    }
    for (const child of [...(n.nodes ?? []), ...(n.floating_nodes ?? [])]) {
      const found = visit(child);
      if (found) return found;
    }
    return null;
  };
  return visit(root);
}

/**
 * The argv that asks i3 to run a command.
 *
 * The command goes in as **separate arguments**, and that is not cosmetic. Handed
 * to `i3-msg` as one argument — `i3-msg "exec -- alacritty …"` — i3-msg reports
 * success and i3 runs *nothing at all*: no error, no window, no trace. Passed as
 * separate arguments, the same command works. Measured directly, same session,
 * same class, back to back:
 *
 *     i3-msg "exec -- alacritty …"        -> no window
 *     i3-msg exec -- "alacritty …"        -> window
 *
 * Each element is still shell-quoted, because `i3-msg` joins its arguments back
 * into one string and i3 runs that through `sh -c`.
 */
export function liveExecArgv(command: readonly string[]): string[] {
  return ["i3-msg", "exec", "--", ...command.map(shellQuote)];
}

/**
 * Launch a session's live window and put it on a chosen workspace.
 *
 * The launch goes through `i3-msg exec` (which is how a window gets made at all
 * under i3) but the *placement* is [placeLive], so the user's view never moves.
 */
export async function openLive(
  tmuxSocket: string,
  tmuxName: string,
  sessionId: string,
  workspace: string,
  tmuxBin = "tmux",
  stateDir?: string,
): Promise<{ ok: boolean; detail?: string; windowId?: string }> {
  const target = safeWorkspace(workspace);
  if (target === null) return { ok: false, detail: "invalid workspace name" };
  if (!process.env.DISPLAY) return { ok: false, detail: "no DISPLAY (not an X11 session)" };
  if (!stateDir) return { ok: false, detail: "no state directory for the live shim" };

  const spec = await prepareLive(stateDir, tmuxSocket, tmuxName, sessionId, tmuxBin);
  const exec = await run(liveExecArgv(spec.argv), { timeoutMs: 3000 });
  if (exec.code !== 0) return { ok: false, detail: "i3-msg not available" };
  return placeLive(sessionId, target);
}

/**
 * Open the mirror via i3 (X11). Returns `ok: false` with a detail message when
 * i3 is unavailable — the caller treats this as best-effort, never fatal.
 *
 * Superseded for phone use by [openLive]: this one moves the user's view, which
 * is why it is no longer what the app calls.
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