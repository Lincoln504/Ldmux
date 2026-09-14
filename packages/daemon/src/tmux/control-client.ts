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
  /** Called with the client that exited, so a superseded one cannot be confused
   * with the current one. */
  onExit?: (client: TmuxControlClient) => void;
  onStderr?: (chunk: string) => void;
}

interface Waiter {
  seq: number;
  timer: ReturnType<typeof setTimeout>;
  resolve: (lines: string[]) => void;
  reject: (err: Error) => void;
}

function quote(arg: string): string {
  return `'${arg.replaceAll("'", "'\\''")}'`;
}

/** Minimum spacing between resumes of the same pane. */
const PAUSE_CONTINUE_MIN_MS = 500;
/**
 * No tmux command may pend forever. A lost response desynchronizes sequence
 * matching, so a timed-out command also tears the client down (the caller sees
 * an error, subscribers resync, and the stream reconnects).
 */
export const COMMAND_TIMEOUT_MS = 15_000;

/**
 * Which pending waiter a response block belongs to, or -1 to ignore the block.
 *
 * Pure so the anti-hang property is directly testable: after attach, a response
 * must always resolve someone (a mis-association fails loudly; a dropped block
 * hangs a caller forever). Before attach the one block that may be ignored is
 * tmux's unsolicited empty `%begin/%end`.
 */
export function matchWaiter(waiterSeqs: readonly number[], seq: number, attached: boolean): number {
  if (seq >= 0) {
    const i = waiterSeqs.indexOf(seq);
    if (i >= 0) return i;
  }
  if (!attached) return -1;
  return waiterSeqs.length > 0 ? 0 : -1;
}

/**
 * Coalesces resumes of a paused pane onto a trailing timer.
 *
 * Throttling by *dropping* the resume would be worse than useless: tmux emits
 * `%pause` only on the transition into the paused state, so a dropped resume
 * leaves the pane frozen until tmux's own control-client timeout. Coalescing
 * guarantees exactly one resume per window, whichever pause arrived last.
 */
export class ContinueCoalescer {
  private readonly pending = new Map<string, ReturnType<typeof setTimeout>>();

  constructor(
    private readonly send: (pane: string) => void,
    private readonly delayMs = PAUSE_CONTINUE_MIN_MS,
  ) {}

  request(pane: string): void {
    if (this.pending.has(pane)) return;
    const timer = setTimeout(() => {
      this.pending.delete(pane);
      this.send(pane);
    }, this.delayMs);
    this.pending.set(pane, timer);
  }

  get pendingCount(): number {
    return this.pending.size;
  }

  clear(): void {
    for (const timer of this.pending.values()) clearTimeout(timer);
    this.pending.clear();
  }
}

export class TmuxControlClient {
  private readonly parser: ControlParser;
  private readonly waiters: Waiter[] = [];
  private nextSeq = 1;
  /** Set by the first `%session-changed`: before it, an unnumbered block is
   * tmux's unsolicited attach block and must not steal a command's response. */
  private attached = false;
  private readonly continuer = new ContinueCoalescer((pane) => {
    if (this.closed) return;
    void this.command(["refresh-client", "-A", `${pane}:continue`]).catch(() => {
      /* the client may be closing; a failed resume surfaces as stream exit */
    });
  });
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
        opts.onExit?.(this);
      },
      onResponse: (seq, ok, lines) => {
        const waiter = this.takeWaiter(seq);
        if (!waiter) return;
        clearTimeout(waiter.timer);
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
   * preferred match is by sequence. If that fails we must still always resolve
   * *something*: a waiter that is never resolved leaves a caller hanging and
   * desyncs the counter, so the fallback is ordered (tmux responses are in
   * order) for every block except the unsolicited empty one tmux emits on
   * attach — and that can only arrive before `%session-changed`, i.e. before
   * `attached` is set. Dropping blocks after that point would trade a
   * mis-association (loud) for a hang (silent).
   */
  private takeWaiter(seq: number): Waiter | undefined {
    const i = matchWaiter(
      this.waiters.map((w) => w.seq),
      seq,
      this.attached,
    );
    return i < 0 ? undefined : this.waiters.splice(i, 1)[0];
  }

  /**
   * tmux discards a paused pane's queued output, so the loss is surfaced
   * (subscribers resync) and the pane is resumed through the coalescer.
   */
  private handlePause(pane: string): void {
    this.opts.onPaused?.(pane);
    this.continuer.request(pane);
  }
  private handleNotification(line: string): void {
    if (line.startsWith("%session-changed")) this.attached = true;
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
      const timer = setTimeout(() => {
        const i = this.waiters.findIndex((w) => w.timer === timer);
        if (i >= 0) this.waiters.splice(i, 1);
        reject(new Error(`tmux command timed out after ${COMMAND_TIMEOUT_MS}ms`));
        // The response was lost, so our counter no longer agrees with tmux's:
        // tear the client down and let the stream resync from a snapshot.
        this.close();
      }, COMMAND_TIMEOUT_MS);
      this.waiters.push({ seq, timer, resolve, reject });
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
    while (this.waiters.length) {
      const waiter = this.waiters.shift()!;
      clearTimeout(waiter.timer);
      waiter.reject(err);
    }
  }

  close(): void {
    this.closed = true;
    this.continuer.clear();
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