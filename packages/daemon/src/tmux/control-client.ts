/**
 * A live tmux control-mode client: spawns `tmux -L <socket> -C attach -t <session>`
 * with plain pipes (no PTY — see docs/adr/0001), drives it with commands, and
 * exposes decoded pane output.
 *
 * Control-mode input is parsed by tmux's command parser, so arguments are
 * shell-quoted when commands are passed as arrays.
 */

import { ControlParser } from "./control-parser.ts";

export interface TmuxControlClientOptions {
  onOutput?: (pane: string, text: string) => void;
  onNotification?: (line: string) => void;
  /** tmux paused a pane because this client fell behind (`%pause`). */
  onPaused?: (pane: string) => void;
  onExit?: () => void;
  onStderr?: (chunk: string) => void;
}

interface Waiter {
  seq: number;
  resolve: (lines: string[]) => void;
  reject: (err: Error) => void;
}

function quote(arg: string): string {
  return `'${arg.replaceAll("'", "'\\''")}'`;
}

/** Minimum spacing between resumes of the same pane. */
export const PAUSE_CONTINUE_MIN_MS = 500;

export class TmuxControlClient {
  private readonly parser: ControlParser;
  private readonly waiters: Waiter[] = [];
  private nextSeq = 1;
  /** Throttles `refresh-client -A pane:continue` so a pause storm cannot thrash. */
  private readonly lastContinue = new Map<string, number>();
  private readonly noteWaiters: Array<{ pred: (line: string) => boolean; resolve: () => void; timer: any }> = [];
  private closed = false;
  readonly exited: Promise<number>;

  private constructor(
    private readonly proc: any,
    private readonly opts: TmuxControlClientOptions,
  ) {
    this.parser = new ControlParser({
      onOutput: opts.onOutput,
      onNotification: (line) => this.handleNotification(line),
      onExit: () => {
        this.rejectAll(new Error("control client exited"));
        opts.onExit?.();
      },
      onResponse: (seq, ok, lines) => {
        const waiter = this.takeWaiter(seq);
        if (!waiter) return;
        if (ok) waiter.resolve(lines);
        else waiter.reject(new Error(lines.join("\n") || "tmux command failed"));
      },
      onPause: (pane) => this.handlePause(pane),
    });
    void this.readLoop();
    void this.stderrLoop();
    this.exited = proc.exited;
  }

  static async connect(
    socket: string,
    session: string,
    opts: TmuxControlClientOptions = {},
  ): Promise<TmuxControlClient> {
    const proc = Bun.spawn(["tmux", "-L", socket, "-C", "attach", "-t", session], {
      stdin: "pipe",
      stdout: "pipe",
      stderr: "pipe",
    });
    const client = new TmuxControlClient(proc, opts);
    // tmux emits an unsolicited empty `%begin/%end` block on attach; wait for
    // `%session-changed` (which follows it) before issuing commands so the
    // response FIFO is not desynchronized.
    await client.waitForNotification((l) => l.startsWith("%session-changed"), 1000).catch(() => {});
    // Enable flow control; ignore failure on very old tmux.
    await client.command(["refresh-client", "-f", "pause-after=2"]).catch(() => {});
    return client;
  }

  /**
   * Correlate a response block with the command that caused it.
   *
   * tmux numbers responses per client (`%begin <ts> <seq> <flags>`), so the
   * preferred match is by sequence. Two safety nets remain because a control
   * stream must never desynchronize: an unnumbered/stale block falls back to
   * the oldest waiter (tmux responses are ordered), and a block whose sequence
   * predates every waiter is ignored — that is the unsolicited empty block tmux
   * emits on attach, which must not steal the first command's response.
   */
  private takeWaiter(seq: number): Waiter | undefined {
    if (seq >= 0) {
      const i = this.waiters.findIndex((w) => w.seq === seq);
      if (i >= 0) return this.waiters.splice(i, 1)[0];
      const oldest = this.waiters[0];
      if (oldest && seq < oldest.seq) return undefined;
    }
    return this.waiters.shift();
  }

  /**
   * A paused pane has stopped streaming and its queued output is discarded, so
   * subscribers must resync from a snapshot; then resume the pane.
   */
  private handlePause(pane: string): void {
    this.opts.onPaused?.(pane);
    const now = Date.now();
    const last = this.lastContinue.get(pane) ?? 0;
    if (now - last < PAUSE_CONTINUE_MIN_MS) return;
    this.lastContinue.set(pane, now);
    void this.command(["refresh-client", "-A", `${pane}:continue`]).catch(() => {
      /* the client may be closing; a failed resume surfaces as stream exit */
    });
  }
  private handleNotification(line: string): void {
    this.opts.onNotification?.(line);
    for (let i = this.noteWaiters.length - 1; i >= 0; i--) {
      const w = this.noteWaiters[i];
      if (w.pred(line)) {
        clearTimeout(w.timer);
        this.noteWaiters.splice(i, 1);
        w.resolve();
      }
    }
  }

  /** Resolve when a `%...` notification matching `pred` arrives, or on timeout. */
  waitForNotification(pred: (line: string) => boolean, ms = 1000): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => {
        const i = this.noteWaiters.findIndex((w) => w.timer === timer);
        if (i >= 0) this.noteWaiters.splice(i, 1);
        reject(new Error("notification timeout"));
      }, ms);
      this.noteWaiters.push({ pred, resolve, timer });
    });
  }

  private async readLoop(): Promise<void> {
    try {
      for await (const chunk of this.proc.stdout as AsyncIterable<Uint8Array>) {
        this.parser.feed(chunk);
      }
    } finally {
      this.rejectAll(new Error("control stream ended"));
    }
  }

  private async stderrLoop(): Promise<void> {
    const dec = new TextDecoder();
    try {
      for await (const chunk of this.proc.stderr as AsyncIterable<Uint8Array>) {
        this.opts.onStderr?.(dec.decode(chunk));
      }
    } catch {
      /* ignore */
    }
  }

  /**
   * Send a tmux command. Pass a raw string (pre-quoted) or an argv array
   * (each element shell-quoted).
   */
  async command(cmd: string | string[]): Promise<string[]> {
    if (this.closed) throw new Error("control client is closed");
    const line = Array.isArray(cmd) ? cmd.map(quote).join(" ") : cmd;
    const seq = this.nextSeq++;
    const promise = new Promise<string[]>((resolve, reject) => {
      this.waiters.push({ seq, resolve, reject });
    });
    const sink = this.proc.stdin;
    await sink.write(line + "\n");
    await sink.flush();
    return promise;
  }

  /** Type literal text into a pane (no key interpretation). */
  async sendText(target: string, text: string): Promise<string[]> {
    return this.command(["send-keys", "-t", target, "-l", text]);
  }

  /** Send named keys (e.g. "Enter", "C-c", "Up") into a pane. */
  async sendKey(target: string, ...keys: string[]): Promise<string[]> {
    return this.command(["send-keys", "-t", target, ...keys]);
  }

  private rejectAll(err: Error): void {
    while (this.waiters.length) this.waiters.shift()!.reject(err);
  }

  close(): void {
    this.closed = true;
    try {
      this.proc.stdin.end();
    } catch {
      /* ignore */
    }
    try {
      this.proc.kill();
    } catch {
      /* ignore */
    }
  }
}