/**
 * TmuxPipeClient: pane output as a raw byte stream, read out of a FIFO.
 *
 * tmux's control mode is the incumbent transport (`control-client.ts`), and it
 * carries `%output` — the pane's bytes octal-escaped inside a line-oriented,
 * unspecified text protocol. This client gets the same bytes a different way:
 * `pipe-pane` runs a command with the pane's output as its stdin, so a `cat`
 * pointed at a FIFO hands us the stream with **no escaping at all**.
 *
 * Measured (docs/TMUX-DEPENDENCY.md §8), on an isolated scratch server:
 *
 *   - Byte-verbatim: real ESC bytes, OSC 52, OSC 8, multibyte, emoji. No octal.
 *   - Lossless under a stalled reader: 400,000 numbered lines with 0 missing.
 *   - Bounded buffer: the server's RSS rose 8.4 MB -> 26 MB and stopped, and the
 *     pane's own process then blocked writing to its pty. That is backpressure,
 *     not loss, and it is where `pause-after` gets its effect too.
 *   - Strictly ordered, and `capture-pane` still works while a pipe is live.
 *   - **tmux does not notice its reader died** (`#{pane_pipe}` still said `1`),
 *     so this client must own that: it polls, and it says so.
 *
 * Why a FIFO rather than a file: a file grows without bound and a `yes` would
 * fill the disk, while a FIFO applies real backpressure and never touches disk.
 *
 * Why the pipe is only started when somebody is watching: tmux is *already* the
 * durable state holder — the pane's grid and its `history-limit` scrollback
 * survive with no pipe at all. Streaming is for the viewer, not for storage, so
 * with no viewer the correct thing is to stop the pipe and let tmux hold the
 * screen. That also means a daemon that dies cannot freeze a pane: it stops
 * reading, so it stops the pipe, and the shell keeps running.
 */

import { spawn } from "node:child_process";
import { createHash } from "node:crypto";
import * as fs from "node:fs";
import * as net from "node:net";
import * as path from "node:path";
import { StringDecoder } from "node:string_decoder";

/** A pipe that dies this often is not worth restarting. */
export const PIPE_READER_POLL_MS = 500;
/**
 * Consecutive negative liveness samples before the reader is called dead.
 *
 * One is not enough: `pipe-pane -o` returns before tmux has necessarily
 * registered the pipe, so an immediate first sample can read `#{pane_pipe}` as 0
 * on a perfectly healthy pipe and tear the stream down at birth.
 */
const PIPE_DEAD_SAMPLES = 2;

export interface TmuxPipeClientOptions {
  tmuxSocket: string;
  sessionName: string;
  /** Where FIFOs live. Must be private to the user. */
  pipeDir: string;
  bin?: string;
  /** The tmux server profile, so a pipe cannot start a server without it. */
  profilePath?: string;
  timeoutMs?: number;
  onOutput(text: string): void;
  onExit(client: TmuxPipeClient): void;
}

const DEFAULT_TIMEOUT_MS = 5000;

/** FIFO path for a session. Hashed, because a session name is not a path. */
export function pipeFifoPath(pipeDir: string, sessionName: string): string {
  const id = createHash("sha256").update(sessionName).digest("hex").slice(0, 16);
  return path.join(pipeDir, `pipe-${id}.fifo`);
}

/** tmux runs the pipe command through `sh -c`, so the path must be shell-quoted. */
function shQuote(s: string): string {
  return `'${s.replaceAll("'", "'\\''")}'`;
}

export class TmuxPipeClient {
  private readonly opts: Required<Pick<TmuxPipeClientOptions, "tmuxSocket" | "sessionName" | "pipeDir">> &
    TmuxPipeClientOptions;
  private readonly fifoPath: string;
  private fd: number | null = null;
  private sock: net.Socket | null = null;
  private readonly decoder = new StringDecoder("utf8");
  private closed = false;
  private started = false;
  private readerAlive = false;
  private deadSamples = 0;
  private watchTimer: ReturnType<typeof setInterval> | null = null;

  /** Bytes handed to `onOutput`. Lets a caller drop a pre-snapshot prefix. */
  bytesOut = 0;

  private constructor(opts: TmuxPipeClientOptions) {
    this.opts = opts as TmuxPipeClient["opts"];
    this.fifoPath = pipeFifoPath(opts.pipeDir, opts.sessionName);
  }

  private get bin(): string {
    return this.opts.bin ?? "tmux";
  }

  private baseArgs(): string[] {
    const args: string[] = [];
    if (this.opts.profilePath) args.push("-f", this.opts.profilePath);
    args.push("-L", this.opts.tmuxSocket);
    return args;
  }

  /** One tmux command, awaited. Returns null on failure rather than throwing. */
  private async tmux(args: string[]): Promise<string | null> {
    return new Promise((resolve) => {
      const child = spawn(this.bin, [...this.baseArgs(), ...args], {
        stdio: ["ignore", "pipe", "pipe"],
      });
      let out = "";
      child.stdout?.on("data", (d: Buffer) => (out += d.toString("utf8")));
      child.stderr?.on("data", () => {});
      const timer = setTimeout(() => {
        child.kill("SIGKILL");
        resolve(null);
      }, this.opts.timeoutMs ?? DEFAULT_TIMEOUT_MS);
      child.on("error", () => {
        clearTimeout(timer);
        resolve(null);
      });
      child.on("close", (code) => {
        clearTimeout(timer);
        resolve(code === 0 ? out : null);
      });
    });
  }

  static async connect(opts: TmuxPipeClientOptions): Promise<TmuxPipeClient> {
    const client = new TmuxPipeClient(opts);
    await client.start();
    return client;
  }

  /**
   * `mkfifo(1)`. Node exposes no `mkfifo` and no socketpair, so the FIFO is made
   * by the coreutils binary. The daemon is Linux-only by design
   * (docs/SPEC.md), so this is not a portability hole — and it is the one
   * external command the pipe path needs beyond tmux itself.
   */
  private mkfifo(): Promise<void> {
    return new Promise((resolve, reject) => {
      const child = spawn("mkfifo", ["-m", "600", this.fifoPath], { stdio: "ignore" });
      child.on("error", (err) => reject(err));
      child.on("close", (code) =>
        code === 0 ? resolve() : reject(new Error(`mkfifo exited ${code}`)),
      );
    });
  }

  private async start(): Promise<void> {
    await fs.promises.mkdir(this.opts.pipeDir, { recursive: true, mode: 0o700 });
    // A FIFO left by a previous daemon is stale by definition: nothing is
    // holding the write end, and reusing it would attach us to a dead reader.
    await fs.promises.rm(this.fifoPath, { force: true });
    await this.mkfifo();

    // The read end is opened BEFORE the pipe command is issued, with
    // `O_RDONLY | O_NONBLOCK`. Opening a FIFO read end blocks in open(2) until a
    // writer appears, and the writer is a tmux job we have not started yet.
    //
    // The fd is read through a `net.Socket` rather than an `fs` stream on
    // purpose. Both alternatives were tried and both fail:
    //   - an `fs.ReadStream` over a non-blocking fd surfaces `EAGAIN` as an
    //     error instead of polling;
    //   - an `fs.ReadStream` over a blocking fd parks a threadpool thread per
    //     pane, and this daemon holds one pipe per viewed pane.
    // A `net.Socket` over the fd polls in the event loop, which is what a pipe
    // actually is. Measured: it delivers the bytes verbatim.
    this.fd = fs.openSync(this.fifoPath, fs.constants.O_RDONLY | fs.constants.O_NONBLOCK);
    this.sock = new net.Socket({ fd: this.fd, readable: true, writable: false });
    this.sock.on("data", (chunk: Buffer) => {
      if (this.closed) return;
      const text = this.decoder.write(chunk);
      if (!text) return;
      // `text` is `StringDecoder` output, so `.length` counts UTF-16 code units,
      // not bytes. The field is named for bytes and its doc comment says bytes, and
      // the same accounting in `remote.ts` uses `Buffer.byteLength` for exactly this
      // reason. Nothing reads this field yet, which is why it is a trap for the next
      // reader rather than a live defect.
      this.bytesOut += Buffer.byteLength(text);
      this.opts.onOutput(text);
    });
    // EOF, and why this handler is not the liveness signal it looks like.
    //
    // The comment here used to say that EOF "also happens when no writer is attached, so it
    // is not proof of death". That is true of the *first* EOF and misleading about the rest:
    // **measured 2026-09-20, a FIFO read end does not report EOF when its last writer is
    // killed.** With the `cat` reader SIGKILLed, `#{pane_pipe}` went on reporting `1` *and*
    // the socket stayed quiet for 15 seconds without an `end` — so neither available signal
    // notices, and the client goes on believing it is streaming while nothing arrives. A
    // frozen terminal with no resync, which is the same failure the control transport had
    // and reachable the same way.
    //
    // The handler stays because it costs nothing and covers a socket closed some other way.
    // Liveness is still read from tmux, and tmux cannot see this one. A first attempt at
    // closing it combined EOF with the tmux poll and did not fire, so it was reverted rather
    // than shipped: docs/STATE.md §7.3 records the gap, the measurement, and the one
    // mechanism that would actually answer it (asking whether the reader process exists,
    // which is what the test for this does).
    this.sock.on("end", () => this.checkReader());
    this.sock.on("error", () => this.checkReader());

    const cmd = `cat > ${shQuote(this.fifoPath)}`;
    const started = await this.tmux(["pipe-pane", "-t", this.opts.sessionName, "-o", cmd]);
    if (started === null) {
      await this.teardown();
      throw new Error("pipe-pane could not be started");
    }
    this.started = true;
    // Confirm tmux actually took the pipe before declaring the client live.
    // `pipe-pane` returning success means the command was accepted, which is not
    // the same as the pipe being registered — and everything downstream assumes a
    // live stream. Bounded, so a pane that will not take a pipe fails loudly here
    // rather than looking like a terminal that has gone silent.
    let registered = false;
    for (let i = 0; i < 20; i++) {
      if (await this.pipeReported()) {
        registered = true;
        break;
      }
      await new Promise((r) => setTimeout(r, 50));
    }
    if (!registered) {
      await this.teardown();
      throw new Error("pipe-pane was accepted but never registered");
    }
    this.readerAlive = true;
    this.deadSamples = 0;
    // tmux does not report a dead reader, so this is how we find out.
    this.watchTimer = setInterval(() => this.checkReader(), PIPE_READER_POLL_MS);
    this.watchTimer.unref?.();
  }

  /** True when tmux still has a pipe on this pane. */
  private async pipeReported(): Promise<boolean> {
    const out = await this.tmux([
      "display-message",
      "-p",
      "-t",
      this.opts.sessionName,
      "#{pane_pipe}",
    ]);
    return out?.trim() === "1";
  }

  private checkReader(): void {
    if (this.closed || !this.started) return;
    void this.pipeReported().then((alive) => {
      if (this.closed || !this.started) return;
      if (alive) {
        this.deadSamples = 0;
        return;
      }
      this.deadSamples++;
      if (this.readerAlive && this.deadSamples >= PIPE_DEAD_SAMPLES) {
        this.readerAlive = false;
        this.opts.onExit(this);
      }
    });
  }

  /**
   * `cat` on this pane died or was toggled off. The pane is untouched (measured:
   * it stays responsive and keeps its history), so this is a stream interruption
   * and not a session loss.
   */
  get alive(): boolean {
    return this.started && this.readerAlive && !this.closed;
  }

  private async teardown(): Promise<void> {
    if (this.watchTimer) clearInterval(this.watchTimer);
    this.watchTimer = null;
    if (this.started) {
      // Toggle the pipe off, so tmux stops buffering for a reader that is gone.
      await this.tmux(["pipe-pane", "-t", this.opts.sessionName]);
      this.started = false;
    }
    try {
      this.sock?.destroy();
    } catch {
      /* ignore */
    }
    this.sock = null;
    if (this.fd !== null) {
      try {
        fs.closeSync(this.fd);
      } catch {
        /* ignore */
      }
      this.fd = null;
    }
    await fs.promises.rm(this.fifoPath, { force: true }).catch(() => {});
  }

  close(): void {
    if (this.closed) return;
    this.closed = true;
    void this.teardown();
  }
}