/**
 * Thin wrapper over the tmux CLI for the ldmux private server.
 *
 * Every call is **asynchronous**: a synchronous spawn blocks the daemon's single
 * event loop for the whole lifetime of the tmux process (measured ~3 ms per
 * `capture-pane`), which would stall every WebSocket reader and every other
 * client. Live streaming is handled separately by the control-mode client
 * (see control-client.ts).
 */

import { existsSync, readFileSync } from "node:fs";
import { spawnCaptured } from "../proc.ts";

/**
 * The shell a user's sessions should get.
 *
 * tmux runs `default-shell`, which tmux takes from `$SHELL`. When the daemon is
 * started from an environment whose `$SHELL` is *not* the user's login shell
 * (e.g. a nix devshell's minimal bash, which may lack builtins the system's
 * completion scripts call), every new session is broken. Prefer the login shell
 * from `/etc/passwd`, then `$SHELL`, then `/bin/bash` — each verified to exist.
 *
 * `passwd` is injectable for tests; the real one is read from disk.
 */
export function resolveLoginShell(
  uid: number,
  env: NodeJS.ProcessEnv = process.env,
  passwd?: string,
): string | undefined {
  let content = passwd;
  if (content === undefined) {
    try {
      content = readFileSync("/etc/passwd", "utf8");
    } catch {
      content = "";
    }
  }
  const line = content.split("\n").find((l) => l.split(":")[2] === String(uid));
  const fromPasswd = line?.split(":")[6]?.trim();
  if (fromPasswd && existsSync(fromPasswd)) return fromPasswd;
  const fromEnv = env.SHELL?.trim();
  if (fromEnv && existsSync(fromEnv)) return fromEnv;
  return existsSync("/bin/bash") ? "/bin/bash" : undefined;
}

export interface TmuxSession {
  name: string;
  windows: number;
  created: number;
  attached: number;
  activity: number;
}

export interface TmuxPane {
  id: string;
  command: string;
  cwd: string;
  dead: boolean;
  pid: number;
  width: number;
  height: number;
}

export interface NewSessionOptions {
  name: string;
  cwd: string;
  command?: string[];
  cols?: number;
  rows?: number;
  /** Environment variables injected into the session's command (tmux -e). */
  env?: Record<string, string>;
  /**
   * Destroy the session when its last client detaches. Used for a session that
   * exists only as long as a desktop terminal window is open on it.
   */
  destroyUnattached?: boolean;
}

export interface CaptureOptions {
  escapes?: boolean;
  /** `true` captures the full bounded history; a number captures that many lines. */
  history?: boolean | number;
  /** Capture the alternate screen (`-a`) — required for full-screen TUIs. */
  alternate?: boolean;
  /**
   * Join rows the terminal wrapped (`-J`, tmux 3.2+). On by default, and the
   * caller should leave it on: without it a wrapped long line is
   * indistinguishable from a real line break, and a client cannot reproduce the
   * pane's wrap state (which is what makes deleting back past a wrap work).
   */
  joinWrapped?: boolean;
}

/** Upper bound for a history capture: unbounded `-S -` blocks the event loop. */
export const HISTORY_CAPTURE_LINES = 5000;

/** A wedged tmux must not pin a request (or a connection) forever. */
export const TMUX_COMMAND_TIMEOUT_MS = 15_000;
/** Grace between the polite SIGTERM and the SIGKILL that follows it. */
export const TMUX_KILL_GRACE_MS = 2000;

/**
 * Bounds for a client-requested pane size. The floor keeps a pane usable (a
 * client bugs out at 1x1 and the shell is unrecoverable); the ceiling is well
 * above any real display and stops a bad client from asking tmux for a window
 * whose capture would blow the stream's byte budget.
 */
export const MIN_PANE_COLS = 20;
export const MAX_PANE_COLS = 500;
export const MIN_PANE_ROWS = 5;
export const MAX_PANE_ROWS = 300;

function clampSize(value: number, min: number, max: number): number {
  const n = Math.trunc(value);
  if (!Number.isFinite(n)) throw new Error(`invalid size: ${value}`);
  return Math.min(max, Math.max(min, n));
}

/** tmux session names we accept (adopt/register): conservative and argv-safe. */
const SESSION_NAME_RE = /^[A-Za-z0-9_-]{1,128}$/;
export function isValidSessionName(name: string): boolean {
  return SESSION_NAME_RE.test(name);
}

export class TmuxControlPlane {
  readonly socket: string;
  private readonly timeoutMs: number;
  private readonly graceMs: number;

  constructor(socket: string, timeoutMs = TMUX_COMMAND_TIMEOUT_MS) {
    this.socket = socket;
    this.timeoutMs = timeoutMs;
    this.graceMs = Math.min(TMUX_KILL_GRACE_MS, Math.max(50, Math.floor(timeoutMs / 2)));
  }

  /**
   * Run one tmux command to completion.
   *
   * Always settles: SIGTERM at `timeoutMs`, SIGKILL at `timeoutMs + graceMs`,
   * and a hard deadline past both so a process that ignores signals cannot pin
   * the caller (handlers are serialized per connection, so a hang here would
   * wedge that connection for good).
   */
  private async run(args: string[]): Promise<{ code: number; out: string; err: string }> {
    const proc = spawnCaptured(["tmux", "-L", this.socket, ...args], {
      // Explicit env, not the default: a child must see the environment as it is
      // now, so a PATH changed at runtime (a stubbed `tmux` in tests, or a user's
      // PATH fix) is honoured rather than a snapshot from process start.
      env: process.env,
    });
    const timeoutErr = `tmux timed out after ${this.timeoutMs}ms`;
    const kill = (signal?: number) => proc.kill(signal);

    let timedOut = false;
    const termTimer = setTimeout(() => {
      timedOut = true;
      kill();
    }, this.timeoutMs);
    const killTimer = setTimeout(() => kill(9), this.timeoutMs + this.graceMs);
    let hardTimer: ReturnType<typeof setTimeout> | undefined;
    const hardDeadline = new Promise<null>((resolve) => {
      hardTimer = setTimeout(() => resolve(null), this.timeoutMs + this.graceMs + 500);
    });

    try {
      // Drain both pipes concurrently: reading them serially can deadlock when
      // one fills while we wait on the other.
      const drained = Promise.all([proc.drained, proc.exited]).then(([{ out, err }, code]) => ({
        out,
        err,
        code,
      }));
      // If we abandon it below, it must not surface as an unhandled rejection.
      drained.catch(() => {});

      const result = await Promise.race([drained, hardDeadline]);
      if (!result) return { code: -1, out: "", err: timeoutErr };
      if (timedOut) return { code: -1, out: result.out, err: timeoutErr };
      // A binary that could not be started at all (ENOENT) has to be an error,
      // not a silent "exit 0 with no output": its callers act on the output.
      const failure = await proc.spawnError;
      if (failure) throw failure;
      return result;
    } finally {
      clearTimeout(termTimer);
      clearTimeout(killTimer);
      if (hardTimer) clearTimeout(hardTimer);
    }
  }

  /** Apply the canonical server option profile (docs/SPEC.md §5.2). */
  async ensureServer(opts: { historyLimit?: number } = {}): Promise<void> {
    const history = opts.historyLimit ?? 10000;
    const loginShell = resolveLoginShell(typeof process.getuid === "function" ? process.getuid() : 0);
    for (const args of [
      ["set-option", "-g", "history-limit", String(history)],
      ...(loginShell ? [["set-option", "-g", "default-shell", loginShell]] : []),
      ["set-option", "-g", "default-terminal", "tmux-256color"],
      ["set-option", "-g", "mouse", "on"],
      ["set-option", "-g", "status", "off"],
      ["set-option", "-g", "aggressive-resize", "off"],
      ["set-option", "-g", "window-size", "manual"],
      ["set-option", "-g", "set-clipboard", "on"],
      ["set-option", "-g", "focus-events", "on"],
      ["set-option", "-g", "remain-on-exit", "on"],
    ]) {
      // Best-effort: some options may not exist on very old tmux.
      await this.run(args);
    }
  }

  /**
   * List live tmux sessions.
   *
   * Returns `null` when the tmux state is *unknown* (the probe failed), which
   * is deliberately distinct from `[]` ("definitely no sessions"). Callers that
   * reconcile external state must never treat `null` as "everything died".
   */
  async listSessions(): Promise<TmuxSession[] | null> {
    const { code, out, err } = await this.run([
      "list-sessions",
      "-F",
      "#{session_name}|#{session_windows}|#{session_created}|#{session_attached}|#{session_activity}",
    ]);
    if (code !== 0) {
      // A missing server is a legitimate empty world; anything else is unknown.
      return /no server running|no sessions|failed to connect/i.test(err) ? [] : null;
    }
    return out
      .split("\n")
      .filter(Boolean)
      .map((line) => {
        const [name, windows, created, attached, activity] = line.split("|");
        return {
          name,
          windows: Number(windows),
          created: Number(created),
          attached: Number(attached),
          activity: Number(activity),
        };
      });
  }

  async listPanes(session: string): Promise<TmuxPane[]> {
    const { out } = await this.run([
      "list-panes",
      "-t",
      session,
      "-F",
      "#{pane_id}|#{pane_current_command}|#{pane_current_path}|#{pane_dead}|#{pane_pid}|#{pane_width}x#{pane_height}",
    ]);
    return out
      .split("\n")
      .filter(Boolean)
      .map((line) => {
        const [id, command, cwd, dead, pid, size] = line.split("|");
        const [width, height] = size.split("x").map(Number);
        return { id, command, cwd, dead: dead === "1", pid: Number(pid), width, height };
      });
  }

  async hasSession(name: string): Promise<boolean> {
    return (await this.run(["has-session", "-t", name])).code === 0;
  }

  async newSession(opts: NewSessionOptions): Promise<void> {
    const args = ["new-session", "-d", "-s", opts.name, "-c", opts.cwd];
    if (opts.cols && opts.rows) args.push("-x", String(opts.cols), "-y", String(opts.rows));
    for (const [key, value] of Object.entries(opts.env ?? {})) args.push("-e", `${key}=${value}`);
    // `--` before the shell-command: without it a command whose first word
    // begins with `-` is scanned as a tmux option. (RPC-reachable via `command`.)
    if (opts.command?.length) args.push("--");
    const { code, err } = await this.run([...args, ...(opts.command ?? [])]);
    if (code !== 0) throw new Error(`tmux new-session failed: ${err.trim() || code}`);
    if (opts.destroyUnattached) {
      // Session-scoped so it never affects ldmux's own or other sessions.
      await this.run(["set-option", "-t", opts.name, "destroy-unattached", "on"]);
    }
  }

  async killSession(name: string): Promise<void> {
    const { code, err } = await this.run(["kill-session", "-t", name]);
    if (code !== 0) throw new Error(`tmux kill-session failed: ${err.trim() || code}`);
  }

  async sendText(target: string, text: string): Promise<void> {
    // `--` ends tmux's option scanning: without it a literal string beginning
    // with `-` (e.g. a pasted "--help") is read as a flag and rejected.
    const { code, err } = await this.run(["send-keys", "-t", target, "-l", "--", text]);
    if (code !== 0) throw new Error(`tmux send-keys failed: ${err.trim() || code}`);
  }

  async sendKey(target: string, ...keys: string[]): Promise<void> {
    const { code, err } = await this.run(["send-keys", "-t", target, "--", ...keys]);
    if (code !== 0) throw new Error(`tmux send-keys failed: ${err.trim() || code}`);
  }

  /**
   * Write raw bytes to the pane's pty.
   *
   * `send-keys -H` takes hex byte values and writes them verbatim, which is the
   * only way to deliver an escape sequence a *program* is expecting: the named
   * keys above are tmux's vocabulary, and this is the program's.
   */
  async sendBytes(target: string, bytes: Uint8Array): Promise<void> {
    if (bytes.length === 0) return;
    const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0"));
    const { code, err } = await this.run(["send-keys", "-t", target, "-H", ...hex]);
    if (code !== 0) throw new Error(`tmux send-keys -H failed: ${err.trim() || code}`);
  }

  /**
   * Synthesize a mouse click for a program that has enabled SGR mouse reporting.
   *
   * The press and release are separate sequences, sent together: a program sees
   * exactly what a real terminal would have sent it, so its own click-to-position
   * logic runs, and we implement nothing protocol-specific on the far side.
   * Coordinates are 1-based on the wire.
   */
  async sendMouseClick(target: string, col: number, row: number): Promise<void> {
    const x = Math.max(1, Math.floor(col) + 1);
    const y = Math.max(1, Math.floor(row) + 1);
    // CSI < button ; x ; y M  (press, button 0 = left) ... m (release)
    const press = `\u001b[<0;${x};${y}M`;
    const release = `\u001b[<0;${x};${y}m`;
    await this.sendBytes(target, new TextEncoder().encode(press + release));
  }

  /**
   * Set the window (and so its single pane) to an exact size.
   *
   * ldmux runs with `window-size manual`, which means nothing else will ever
   * change a pane's geometry — so the *client* has to, or every session stays
   * at whatever size it was created with regardless of the screen showing it.
   * This is the mobile counterpart of a terminal's own TIOCSWINSZ.
   */
  async resizeWindow(name: string, cols: number, rows: number): Promise<void> {
    const c = clampSize(cols, MIN_PANE_COLS, MAX_PANE_COLS);
    const r = clampSize(rows, MIN_PANE_ROWS, MAX_PANE_ROWS);
    const { code, err } = await this.run([
      "resize-window",
      "-t",
      name,
      "-x",
      String(c),
      "-y",
      String(r),
    ]);
    if (code !== 0) throw new Error(`tmux resize-window failed: ${err.trim() || code}`);
  }

  async capturePane(target: string, opts: CaptureOptions = {}): Promise<string> {
    const withJoined = opts.joinWrapped !== false;
    const res = await this.runCapture(target, opts, withJoined);
    if (res.code === 0) return res.out;
    // `-J` needs tmux 3.2+. A build without it still has to produce a screen,
    // so fall back once rather than failing every snapshot on an old host.
    if (withJoined) {
      const fallback = await this.runCapture(target, opts, false);
      if (fallback.code === 0) return fallback.out;
    }
    throw new Error(`tmux capture-pane failed: ${res.err.trim() || res.code}`);
  }

  private async runCapture(
    target: string,
    opts: CaptureOptions,
    joinWrapped: boolean,
  ): Promise<{ code: number; out: string; err: string }> {
    const args = ["capture-pane", "-p", "-t", target];
    if (opts.escapes !== false) args.push("-e");
    // `-a` captures the alternate screen, which is where full-screen TUIs live.
    if (opts.alternate) args.push("-a");
    // `-J` joins rows the terminal *wrapped*, so a long logical line comes back
    // as one line. Without it every row is a hard newline, and a client cannot
    // tell a wrap from a real line break — which is what breaks the cursor when
    // the user deletes back past the wrap point. (tmux 3.2+.)
    if (joinWrapped) args.push("-J");
    // History is bounded: capturing 50k lines blocks the event loop (see docs/WEB.md).
    if (opts.history) {
      const lines = typeof opts.history === "number" ? opts.history : HISTORY_CAPTURE_LINES;
      args.push("-S", `-${Math.max(1, Math.floor(lines))}`, "-E", "-");
    }
    return this.run(args);
  }

  async displayMessage(target: string, format: string): Promise<string> {
    const { out } = await this.run(["display-message", "-p", "-t", target, format]);
    return out.replace(/\n$/, "");
  }

  /** Terminal state fields for a snapshot (best-effort across tmux versions). */
  async snapshotState(target: string) {
    const fmt =
      "#{cursor_x},#{cursor_y},#{alternate_on},#{pane_in_mode},#{history_size}," +
      "#{pane_width},#{pane_height},#{pane_dead}," +
      // mouse_any_flag and keypad_cursor_flag are old; bracket_paste_flag is tmux
      // 3.8+ and comes back empty on older builds (treated as false).
      "#{mouse_any_flag},#{keypad_cursor_flag},#{bracket_paste_flag}," +
      // SGR (1006) is the only mouse encoding we can synthesize for: the older
      // encodings cap coordinates at 223 and are not worth carrying.
      "#{mouse_sgr_flag}," +
      // Who is actually in the foreground. Needed because the mouse mode is
      // sticky: a program that enabled reporting and then exited leaves it on,
      // so the flags alone would have us writing click bytes to a shell.
      "#{pane_current_command}";
    const line = await this.displayMessage(target, fmt);
    const [cursorX, cursorY, alt, mode, history, width, height, dead, mouse, appCursor, bracketedPaste, mouseSgr, foreground] =
      line.split(",");
    return {
      cursorX: Number(cursorX),
      cursorY: Number(cursorY),
      alternateOn: alt === "1",
      paneInMode: mode === "1",
      mouse: mouse === "1",
      mouseSgr: mouseSgr === "1",
      appCursor: appCursor === "1",
      bracketedPaste: bracketedPaste === "1",
      historySize: Number(history),
      cols: Number(width),
      rows: Number(height),
      dead: dead === "1",
      foreground: String(foreground ?? ""),
    };
  }
}