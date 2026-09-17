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
import { basename, delimiter, join } from "node:path";
import { spawnCaptured } from "../proc.ts";

/**
 * Find the tmux binary to run.
 *
 * `LDMUX_TMUX_BIN` wins, because a systemd unit or an image may know exactly
 * which one it means. Otherwise PATH is searched in order and the absolute path
 * is kept — the path matters as much as the binary: anything this daemon spawns
 * later (a live window on the desktop) inherits a *different* environment, and a
 * bare `tmux` there would resolve to whatever the window manager finds.
 *
 * Falls back to the bare name, so a host where nothing is found behaves exactly
 * as it did before rather than failing here.
 */
export function resolveTmuxBin(
  env: NodeJS.ProcessEnv = process.env,
  exists: (p: string) => boolean = existsSync,
): string {
  const explicit = env.LDMUX_TMUX_BIN?.trim();
  if (explicit && exists(explicit)) return explicit;
  for (const dir of (env.PATH ?? "").split(delimiter)) {
    if (!dir) continue;
    const candidate = join(dir, "tmux");
    if (exists(candidate)) return candidate;
  }
  return "tmux";
}

/**
 * A shell worth handing a person.
 *
 * Bash is the default and zsh is welcome. `sh` and `dash` are skipped on purpose:
 * they have no line editor, so a session running one cannot be walked back or
 * edited at all — which is the state this function exists to prevent.
 */
function isUsableShell(candidate: string | undefined): boolean {
  if (!candidate) return false;
  return existsSync(candidate) && /^(bash|zsh)/.test(basename(candidate));
}

/**
 * The shell a user's sessions should get.
 *
 * tmux runs `default-shell`, which tmux takes from `$SHELL` — and `$SHELL` is
 * not the user's shell when the daemon was started from a devshell, a
 * virtualenv, or a CI image. A nix devshell is the sharp edge: its `$SHELL` is
 * the devshell's own bash, built without readline, so every session it created
 * had no line editing at all — arrow keys echoed as `^[[D`, the cursor could not
 * be walked back past a wrapped line, and `\[`/`\]` were printed literally, so
 * the prompt looked doubled.
 *
 * So the host is asked instead. `/etc/passwd` says which shell this user uses,
 * and that is the one their rc files, aliases and expectations belong to. `$SHELL`
 * is consulted last, only for the case where passwd says nothing useful.
 *
 * Deliberately not a capability probe. Which shell a person runs is the host's
 * business; the job here is to stop a build environment from deciding it.
 *
 * `passwd` is injectable for tests; the real one is read from disk.
 *
 * Returns `undefined` when the host has neither bash nor zsh — a host with only
 * `sh` is left to tmux's own default rather than handed a shell nobody wants.
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
  return [line?.split(":")[6]?.trim(), "/bin/bash", "/usr/bin/bash", env.SHELL?.trim()].find(
    isUsableShell,
  );
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

/**
 * The tmux argv for one prefix action.
 *
 * Pure and exported because the flag is easy to get backwards, and getting it
 * backwards is invisible: both splits succeed, both report success, and only the
 * *shape* of the result is wrong. Measured on tmux 3.7b — tmux's own default is a
 * horizontal *divider*, so the new pane lands **below** and the rows halve;
 * `-h` is what puts panes side by side and halves the columns. Hence the names
 * here say where the pane goes, not which flag it is.
 */
export function tmuxActionArgs(action: Exclude<TmuxAction, "rename">, target: string): string[] {
  switch (action) {
    case "split-right":
      return ["split-window", "-h", "-t", target];
    case "split-down":
      return ["split-window", "-t", target];
    case "new-window":
      return ["new-window", "-t", target];
  }
}

/**
 * The prefix-key actions the phone offers, named for what they do to the screen
 * rather than for tmux's own flags (`split-down` reads better than `-h`, which in
 * tmux means a horizontal split *line* and produces a stacked pair).
 */
export type TmuxAction = "split-right" | "split-down" | "new-window" | "rename";

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
   * Join rows the terminal wrapped (`-J`, tmux 3.2+). **Off by default, and it
   * must stay off for anything a client replays.**
   *
   * This was on by default until it was measured on a device, and the reasoning
   * behind it was exactly backwards. The idea was that a client cannot tell a
   * wrap from a real line break without `-J`. But a client is not *reading* the
   * rows — it is replaying them into an emulator of the same width, and the wrap
   * is a consequence of that width, so it reproduces itself. Joining destroys
   * that: a 105-column line comes back as one row, the client re-wraps it into
   * two, the capture has one row more than the pane does, and the screen scrolls
   * by one — silently deleting the top line. On a 20-row pane whose first row is
   * the prompt, the prompt is simply gone, and what is left looks like a
   * rendering bug rather than a captured one.
   *
   * Turn it on only when the output is text being read rather than a screen being
   * reproduced (`ldmux capture`).
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

  /**
   * The tmux binary this control plane runs.
   *
   * Resolved to an absolute path once, and used for every spawn *and* for any
   * client this daemon launches on the desktop. A bare `tmux` is not good enough
   * for either: the daemon may be running a tmux from a nix profile or a
   * container while the window manager's environment resolves the system one, and
   * two tmux versions cannot share a server — the second client exits immediately
   * with a protocol mismatch, so a live window would flash and vanish.
   */
  readonly bin: string;

  constructor(socket: string, timeoutMs = TMUX_COMMAND_TIMEOUT_MS, bin?: string) {
    this.socket = socket;
    this.timeoutMs = timeoutMs;
    this.bin = bin ?? resolveTmuxBin();
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
    const proc = spawnCaptured([this.bin, "-L", this.socket, ...args], {
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
   * Enter or leave tmux copy-mode.
   *
   * This is the only way to look at the history of a **full-screen** program.
   * ldmux keeps scrollback for the normal screen, but a program on the
   * alternate screen (vim, htop, less) has no scrollback to keep: what it drew
   * is not in the history at all. copy-mode asks tmux to scroll *its* view of the
   * pane, which is the only copy that exists.
   *
   * Leaving is `send-keys -X cancel` rather than the `q` key, because `q` is
   * copy-mode's emacs-mode binding and a user with vi-mode bindings would find
   * it does something else.
   */
  async copyMode(target: string, on: boolean): Promise<void> {
    const args = on
      ? ["copy-mode", "-t", target]
      : ["send-keys", "-t", target, "-X", "cancel"];
    const { code, err } = await this.run(args);
    if (code !== 0) {
      // Leaving a copy-mode that was never entered is not a failure worth
      // raising: it is the state the caller wanted, and it is reachable by
      // pressing the toggle twice or by whatever the user typed in between.
      if (!on && /not in a mode/i.test(err)) return;
      throw new Error(`tmux copy-mode failed: ${err.trim() || code}`);
    }
  }

  /**
   * A session-level tmux action: the things the prefix key would do if the phone
   * were a tmux client rather than a program inside a pane.
   *
   * The prefix key itself cannot be sent. `send-keys C-b` writes 0x02 into the
   * pane's pty — it reaches the *program*, where it means readline's
   * backward-char, not tmux. The prefix is interpreted by a tmux *client*, and
   * the client here is the daemon, so these are the command directly.
   */
  async tmuxAction(target: string, action: TmuxAction, name?: string): Promise<void> {
    if (action === "rename") return this.renameWindow(target, name ?? "");
    const { code, err } = await this.run(tmuxActionArgs(action, target));
    if (code !== 0) throw new Error(`tmux ${action} failed: ${err.trim() || code}`);
  }

  /**
   * Rename the session's current window.
   *
   * The window, not the session: the phone shows a window, and a rename that
   * changed the machine name of the session would break the thing the client is
   * attached to.
   */
  async renameWindow(target: string, name: string): Promise<void> {
    const clean = name.trim().slice(0, 64);
    if (clean === "") throw new Error("window name is empty");
    const { code, err } = await this.run(["rename-window", "-t", target, "--", clean]);
    if (code !== 0) throw new Error(`tmux rename-window failed: ${err.trim() || code}`);
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
    const withJoined = opts.joinWrapped === true;
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