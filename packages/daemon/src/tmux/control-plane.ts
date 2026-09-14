/**
 * Thin, synchronous wrapper over the tmux CLI for the ldmux private server.
 * Used for the control plane (list/new/kill/send/capture) — live streaming is
 * handled by the control-mode client (see control-client.ts).
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
}

export class TmuxControlPlane {
  constructor(readonly socket: string) {}

  private run(args: string[]) {
    const proc = Bun.spawnSync(["tmux", "-L", this.socket, ...args]);
    return {
      code: proc.exitCode ?? -1,
      out: proc.stdout ? proc.stdout.toString() : "",
      err: proc.stderr ? proc.stderr.toString() : "",
    };
  }

  /** Apply the canonical server option profile (docs/SPEC.md §5.2). */
  ensureServer(opts: { historyLimit?: number } = {}): void {
    const history = opts.historyLimit ?? 50000;
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
      this.run(args);
    }
  }

  listSessions(): TmuxSession[] {
    const { out } = this.run([
      "list-sessions",
      "-F",
      "#{session_name}|#{session_windows}|#{session_created}|#{session_attached}|#{session_activity}",
    ]);
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

  listPanes(session: string): TmuxPane[] {
    const { out } = this.run([
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

  hasSession(name: string): boolean {
    return this.run(["has-session", "-t", name]).code === 0;
  }

  newSession(opts: NewSessionOptions): void {
    const args = ["new-session", "-d", "-s", opts.name, "-c", opts.cwd];
    if (opts.cols && opts.rows) args.push("-x", String(opts.cols), "-y", String(opts.rows));
    for (const [key, value] of Object.entries(opts.env ?? {})) args.push("-e", `${key}=${value}`);
    const { code, err } = this.run([...args, ...(opts.command ?? [])]);
    if (code !== 0) throw new Error(`tmux new-session failed: ${err.trim() || code}`);
  }

  killSession(name: string): void {
    const { code, err } = this.run(["kill-session", "-t", name]);
    if (code !== 0) throw new Error(`tmux kill-session failed: ${err.trim() || code}`);
  }

  sendText(target: string, text: string): void {
    const { code, err } = this.run(["send-keys", "-t", target, "-l", text]);
    if (code !== 0) throw new Error(`tmux send-keys failed: ${err.trim() || code}`);
  }

  sendKey(target: string, ...keys: string[]): void {
    const { code, err } = this.run(["send-keys", "-t", target, ...keys]);
    if (code !== 0) throw new Error(`tmux send-keys failed: ${err.trim() || code}`);
  }

  capturePane(target: string, opts: CaptureOptions = {}): string {
    const args = ["capture-pane", "-p", "-t", target];
    if (opts.escapes !== false) args.push("-e");
    if (opts.history) args.push("-S", "-", "-E", "-");
    const { code, out, err } = this.run(args);
    if (code !== 0) throw new Error(`tmux capture-pane failed: ${err.trim() || code}`);
    return out;
  }

  displayMessage(target: string, format: string): string {
    const { out } = this.run(["display-message", "-p", "-t", target, format]);
    return out.replace(/\n$/, "");
  }

  /** Terminal state fields for a snapshot (best-effort across tmux versions). */
  snapshotState(target: string) {
    const fmt =
      "#{cursor_x},#{cursor_y},#{alternate_on},#{pane_in_mode},#{history_size}," +
      "#{pane_width},#{pane_height},#{pane_dead}";
    const [cursorX, cursorY, alt, mode, history, width, height, dead] = this.displayMessage(target, fmt).split(",");
    return {
      cursorX: Number(cursorX),
      cursorY: Number(cursorY),
      alternateOn: alt === "1",
      paneInMode: mode === "1",
      historySize: Number(history),
      cols: Number(width),
      rows: Number(height),
      dead: dead === "1",
    };
  }
}