/**
 * Thin wrapper over the tmux CLI for the ldmux private server.
 *
 * Every call is **asynchronous**: `Bun.spawnSync` blocks the daemon's single
 * event loop for the whole lifetime of the tmux process (measured ~3 ms per
 * `capture-pane`), which would stall every WebSocket reader and every other
 * client. Live streaming is handled separately by the control-mode client
 * (see control-client.ts).
 */

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
}

export interface CaptureOptions {
  escapes?: boolean;
  history?: boolean;
  /** Capture the alternate screen (`-a`) — required for full-screen TUIs. */
  alternate?: boolean;
}

/** Upper bound for a history capture: unbounded `-S -` blocks the event loop. */
export const HISTORY_CAPTURE_LINES = 5000;

/** A wedged tmux must not pin a request (or a connection) forever. */
export const TMUX_COMMAND_TIMEOUT_MS = 15_000;
/** Grace between the polite SIGTERM and the SIGKILL that follows it. */
export const TMUX_KILL_GRACE_MS = 2000;

/** tmux session names we accept (adopt/register): conservative and argv-safe. */
const SESSION_NAME_RE = /^[A-Za-z0-9_-]{1,128}$/;
export function isValidSessionName(name: string): boolean {
  return SESSION_NAME_RE.test(name);
}

export class TmuxControlPlane {
  private readonly timeoutMs: number;
  private readonly graceMs: number;

  constructor(readonly socket: string, timeoutMs = TMUX_COMMAND_TIMEOUT_MS) {
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
    const proc = Bun.spawn(["tmux", "-L", this.socket, ...args], {
      stdin: "ignore",
      stdout: "pipe",
      stderr: "pipe",
    });
    const timeoutErr = `tmux timed out after ${this.timeoutMs}ms`;
    // kill() returns a boolean for an already-exited child in Bun, but never
    // let a throw in a timer callback become an uncaught exception.
    const kill = (signal?: number) => {
      try {
        if (signal === undefined) proc.kill();
        else proc.kill(signal);
      } catch {
        /* already gone */
      }
    };

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
      const drained = Promise.all([
        new Response(proc.stdout).text(),
        new Response(proc.stderr).text(),
        proc.exited,
      ]).then(([out, err, code]) => ({ out, err, code: code ?? -1 }));
      // If we abandon it below, it must not surface as an unhandled rejection.
      drained.catch(() => {});

      const result = await Promise.race([drained, hardDeadline]);
      if (!result) return { code: -1, out: "", err: timeoutErr };
      if (timedOut) return { code: -1, out: result.out, err: timeoutErr };
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
    for (const args of [
      ["set-option", "-g", "history-limit", String(history)],
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
    const { code, err } = await this.run([...args, ...(opts.command ?? [])]);
    if (code !== 0) throw new Error(`tmux new-session failed: ${err.trim() || code}`);
  }

  async killSession(name: string): Promise<void> {
    const { code, err } = await this.run(["kill-session", "-t", name]);
    if (code !== 0) throw new Error(`tmux kill-session failed: ${err.trim() || code}`);
  }

  async sendText(target: string, text: string): Promise<void> {
    const { code, err } = await this.run(["send-keys", "-t", target, "-l", text]);
    if (code !== 0) throw new Error(`tmux send-keys failed: ${err.trim() || code}`);
  }

  async sendKey(target: string, ...keys: string[]): Promise<void> {
    const { code, err } = await this.run(["send-keys", "-t", target, ...keys]);
    if (code !== 0) throw new Error(`tmux send-keys failed: ${err.trim() || code}`);
  }

  async capturePane(target: string, opts: CaptureOptions = {}): Promise<string> {
    const args = ["capture-pane", "-p", "-t", target];
    if (opts.escapes !== false) args.push("-e");
    // `-a` captures the alternate screen, which is where full-screen TUIs live.
    if (opts.alternate) args.push("-a");
    // History is bounded: capturing 50k lines blocks the event loop (see docs/WEB.md).
    if (opts.history) args.push("-S", `-${HISTORY_CAPTURE_LINES}`, "-E", "-");
    const { code, out, err } = await this.run(args);
    if (code !== 0) throw new Error(`tmux capture-pane failed: ${err.trim() || code}`);
    return out;
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
      "#{mouse_any_flag},#{keypad_cursor_flag},#{bracket_paste_flag}";
    const line = await this.displayMessage(target, fmt);
    const [cursorX, cursorY, alt, mode, history, width, height, dead, mouse, appCursor, bracketedPaste] =
      line.split(",");
    return {
      cursorX: Number(cursorX),
      cursorY: Number(cursorY),
      alternateOn: alt === "1",
      paneInMode: mode === "1",
      mouse: mouse === "1",
      appCursor: appCursor === "1",
      bracketedPaste: bracketedPaste === "1",
      historySize: Number(history),
      cols: Number(width),
      rows: Number(height),
      dead: dead === "1",
    };
  }
}