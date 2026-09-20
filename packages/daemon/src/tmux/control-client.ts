/**
 * A live tmux control-mode client: spawns `tmux -L <socket> -C attach -t <session>`
 * with plain pipes (no PTY — see docs/adr/0001), drives it with commands, and
 * exposes decoded pane output.
 *
 * Control-mode input is parsed by tmux's command parser, so arguments are
 * shell-quoted when commands are passed as arrays.
 */

import { spawn } from "node:child_process";
import { killTree } from "../proc.ts";
import { ControlParser } from "./control-parser.ts";

export interface TmuxControlClientOptions {
  /** The server profile to read, so a server this client starts is not on defaults. */
  profilePath?: string;
  onOutput?: (pane: string, text: string) => void;
  onNotification?: (line: string) => void;
  /** tmux paused a pane because this client fell behind (`%pause`). */
  onPaused?: (pane: string) => void;
  /** Called with the client that exited, so a superseded one cannot be confused
   * with the current one. */
  onExit?: (client: TmuxControlClient) => void;
  onStderr?: (chunk: string) => void;
  /**
   * Seconds of client lag before tmux pauses a pane (passed to
   * `refresh-client -f pause-after=N`). Default 2; 0 makes tmux pause on the
   * slightest lag, which is how the pause/resume path is tested.
   */
  pauseAfter?: number;
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

  private readonly send: (pane: string) => void;
  private readonly delayMs: number;

  constructor(send: (pane: string) => void, delayMs = PAUSE_CONTINUE_MIN_MS) {
    this.send = send;
    this.delayMs = delayMs;
  }

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

export interface ControlProc {
  stdout: AsyncIterable<Uint8Array>;
  stderr: AsyncIterable<Uint8Array>;
  stdin: { write(data: string): Promise<void>; end(): void };
  exited: Promise<number>;
  kill(): void;
  /** The child's own pid, for logs and for a supervisor that has to end it. */
  pid?: number;
}

/**
 * The tmux control-mode child, adapted to the small surface the client needs.
 *
 * A pipe write that reports backpressure is awaited rather than dropped: the
 * control stream is a command/response protocol, so losing a command silently
 * would desynchronise the response FIFO.
 */
function spawnTmuxControl(
  socket: string,
  session: string,
  profilePath?: string,
): ControlProc {
  // `-f` too: a control client can be what starts the server, and a server started
  // without the profile is a server on tmux's defaults.
  const child = spawn(
    "tmux",
    ["-L", socket, ...(profilePath ? ["-f", profilePath] : []), "-C", "attach", "-t", session],
    {
    stdio: ["pipe", "pipe", "pipe"],
    // Its own process group: closing a control client must not leave tmux's
    // helpers behind holding our pipes open.
    detached: true,
  });
  const exited = new Promise<number>((resolve) => {
    child.once("error", () => resolve(-1));
    child.once("close", (code, signal) => resolve(signal ? -1 : (code ?? -1)));
  });
  return {
    stdout: child.stdout!,
    stderr: child.stderr!,
    pid: child.pid,
    stdin: {
      write: (data: string) =>
        new Promise<void>((resolve, reject) => {
          const sink = child.stdin!;
          if (!sink.writable) {
            reject(new Error("control stream is closed"));
            return;
          }
          try {
            if (sink.write(data)) {
              resolve();
              return;
            }
          } catch (err) {
            reject(err as Error);
            return;
          }
          // The kernel buffer is full: wait for it to drain, then the write has
          // been handed off. `error` can fire instead (EPIPE on a dead tmux).
          const onDrain = () => {
            cleanup();
            resolve();
          };
          const onError = (err: Error) => {
            cleanup();
            reject(err);
          };
          const cleanup = () => {
            sink.off("drain", onDrain);
            sink.off("error", onError);
          };
          sink.once("drain", onDrain);
          sink.once("error", onError);
        }),
      end: () => {
        try {
          child.stdin?.end();
        } catch {
          /* already closed */
        }
      },
    },
    exited,
    kill: () => killTree(child),
  };
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
  /** Guards [fireExit]: two independent signals can announce one death. */
  private exitFired = false;
  readonly exited: Promise<number>;

  private readonly proc: ControlProc;
  private readonly opts: TmuxControlClientOptions;

  private constructor(proc: ControlProc, opts: TmuxControlClientOptions) {
    this.proc = proc;
    this.opts = opts;
    this.parser = new ControlParser({
      onOutput: opts.onOutput,
      onNotification: (line) => this.handleNotification(line),
      onExit: () => this.fireExit(),
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
    // The process handle is computed on the line above and, until now, nothing read
    // it. Recovery hung entirely on the parser's `%exit`, which tmux writes when it
    // detaches cleanly and NOT when the process is killed, OOM-killed, or crashes.
    // A client that died that way left `PaneStream` believing it was alive: no
    // `onReset`, no reconnect, no resync, and a terminal frozen forever. That is
    // precisely the failure snapshot-on-connect exists to recover from, and it was
    // unreachable from here.
    void proc.exited.then(() => this.fireExit());
  }

  /**
   * Announce the control client's death exactly once.
   *
   * Two signals can report it and they race: the parser seeing `%exit`, and the
   * process handle resolving. Firing twice would make `PaneStream` reconnect twice
   * and hand every subscriber two resets for one death.
   */
  private fireExit(): void {
    if (this.exitFired) return;
    this.exitFired = true;
    this.rejectAll(new Error("control client exited"));
    this.opts.onExit?.(this);
  }

  /** The control client's own pid, for logs and for a supervisor that must end it. */
  get pid(): number | undefined {
    return this.proc.pid;
  }

  static async connect(
    socket: string,
    session: string,
    opts: TmuxControlClientOptions = {},
  ): Promise<TmuxControlClient> {
    const proc = spawnTmuxControl(socket, session, opts.profilePath);
    const client = new TmuxControlClient(proc, opts);
    // tmux emits an unsolicited empty `%begin/%end` block on attach; wait for
    // `%session-changed` (which follows it) before issuing commands so the
    // response FIFO is not desynchronized.
    await client.waitForNotification((l) => l.startsWith("%session-changed"), 1000).catch(() => {});
    // Enable flow control; ignore failure on very old tmux.
    await client
      .command(["refresh-client", "-f", `pause-after=${opts.pauseAfter ?? 2}`])
      .catch(() => {});
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
        // Streaming decode: a multi-byte character can straddle two reads.
        this.opts.onStderr?.(dec.decode(chunk, { stream: true }));
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
    try {
      await sink.write(line + "\n");
    } catch (err) {
      // The write failed (dead tmux, EPIPE): drop the waiter we just registered
      // and observe the promise, so its rejection cannot surface unhandled when
      // the read loop or the command timer later rejects everything.
      const i = this.waiters.findIndex((w) => w.seq === seq);
      if (i !== -1) {
        clearTimeout(this.waiters[i]!.timer);
        this.waiters.splice(i, 1);
      }
      promise.catch(() => {});
      throw err;
    }
    return promise;
  }

  /** Type literal text into a pane (no key interpretation). */
  async sendText(target: string, text: string): Promise<string[]> {
    // `--` stops tmux option scanning so a literal string starting with `-` is
    // typed rather than rejected as a flag.
    return this.command(["send-keys", "-t", target, "-l", "--", text]);
  }

  /** Send named keys (e.g. "Enter", "C-c", "Up") into a pane. */
  async sendKey(target: string, ...keys: string[]): Promise<string[]> {
    return this.command(["send-keys", "-t", target, "--", ...keys]);
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