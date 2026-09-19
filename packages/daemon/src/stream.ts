/**
 * PaneStream: one live tmux control-mode client per viewed session, fanned out
 * to any number of subscribers (WebSocket clients).
 *
 * Lifecycle: the control client is created on the first subscriber, survives
 * brief subscriber churn (5 s linger), and is torn down when truly idle. If the
 * control stream dies, subscribers are told to resync (`onReset`) and the
 * client is reconnected with bounded backoff; when reconnects are exhausted the
 * stream falls back to `capture-pane` diff polling so a viewer keeps seeing the
 * pane even if control mode cannot run on this machine. Snapshot-on-connect is
 * the correctness backstop (ADR-0001/0004): live output is best-effort.
 */

import { setTimeout as sleep } from "node:timers/promises";
import type { PaneState } from "../../protocol/src/rpc.ts";
import { createSanitizer, sanitizeOnce } from "./sanitize.ts";
import type { Sanitizer } from "./sanitize.ts";
import { TmuxControlClient } from "./tmux/control-client.ts";
import { TmuxPipeClient } from "./tmux/pipe-client.ts";
import type { TmuxControlPlane } from "./tmux/control-plane.ts";
import { screenUpdate } from "./tmux/screen-diff.ts";

export interface StreamSubscriber {
  onOutput(text: string): void;
  onReset(reason: string): void;
}

export interface StreamSnapshot {
  screen: string;
  state: PaneState;
  epoch: number;
  /** True when `screen` includes paged history rather than just the visible screen. */
  history?: boolean;
}

export type StreamMode = "control" | "pipe" | "polling";

/**
 * Which live transport to prefer.
 *
 * `control` is tmux's control-mode `%output`: an ordered, structured stream
 * inside an unspecified text protocol with octal escaping, and the only one of
 * the two with flow control (`pause-after`).
 *
 * `pipe` is `pipe-pane` into a FIFO: byte-verbatim, lossless, bounded, strictly
 * ordered, and with no protocol to parse at all — but with no flow control, and
 * with liveness that tmux does not report.
 *
 * Both remain selected at runtime rather than at build time so the comparison
 * can be made on the phone's own numbers (docs/TMUX-DEPENDENCY.md §7).
 */
export type StreamTransport = "control" | "pipe";

const LINGER_MS = 5000;
const MAX_RECONNECT_ATTEMPTS = 3;
const DEFAULT_POLL_MS = 250;
const CONTROL_RETRY_MS = 30_000;
/** Polled history is bounded like every other capture (see docs/WEB.md). */
export const MAX_HISTORY_LINES = 5000;
/** A pause storm must not spam every subscriber with resyncs. */
const PAUSE_RESET_MIN_MS = 1000;

/**
 * What a pane stream is configured with.
 *
 * Named rather than inline, and that is not style: the field and the constructor both
 * carried a copy of this type, so adding an option to one left the other rejecting it —
 * which is exactly what happened when the server profile was threaded through.
 */
export interface PaneStreamOptions {
  pollMs?: number;
  pauseAfter?: number;
  clipboardWrite?: boolean;
  /** The tmux server profile, so an attach cannot start a server without it. */
  profilePath?: string;
  /**
   * Which live transport to try first. Defaults to `control`, so nothing about
   * the shipped behaviour changes unless it is asked for.
   */
  transport?: StreamTransport;
  /** Where pipe FIFOs live. Required when `transport` is `pipe`. */
  pipeDir?: string;
}

export class PaneStream {
  private client: TmuxControlClient | null = null;
  private pipe: TmuxPipeClient | null = null;
  private connecting: Promise<void> | null = null;
  private readonly subs = new Set<StreamSubscriber>();
  private readonly sanitizer: Sanitizer;
  private lingerTimer: ReturnType<typeof setTimeout> | null = null;
  private closed = false;
  private epoch = 0;
  private reconnectAttempts = 0;
  private polling = false;
  private pollTimer: ReturnType<typeof setTimeout> | null = null;
  private lastPolledScreen = "";
  private lastControlAttempt = 0;
  private lastPauseReset = 0;

  private readonly tmuxSocket: string;
  readonly sessionName: string;
  private readonly cp: TmuxControlPlane;
  private readonly opts: PaneStreamOptions;

  constructor(
    tmuxSocket: string,
    sessionName: string,
    cp: TmuxControlPlane,
    opts: PaneStreamOptions = {},
  ) {
    this.tmuxSocket = tmuxSocket;
    this.sessionName = sessionName;
    this.cp = cp;
    this.opts = opts;
    // Built here, from the option, because an OSC 52 sequence can straddle two
    // output chunks: whether it is dropped has to be decided by the same state
    // machine that is tracking the partial sequence. It defaults to closed.
    this.sanitizer = createSanitizer({ clipboardWrite: opts.clipboardWrite === true });
  }

  get subscriberCount(): number {
    return this.subs.size;
  }

  get mode(): StreamMode {
    if (this.pipe) return "pipe";
    return this.polling ? "polling" : "control";
  }

  /** Subscribe and ensure the control stream is live. Returns an unsubscribe fn. */
  async subscribe(sub: StreamSubscriber): Promise<() => void> {
    if (this.closed) throw new Error("stream closed");
    this.subs.add(sub);
    if (this.lingerTimer) {
      clearTimeout(this.lingerTimer);
      this.lingerTimer = null;
    }
    try {
      await this.ensureConnected();
    } catch (err) {
      this.subs.delete(sub);
      throw err;
    }
    // The stream may have been closed while connecting (session killed).
    if (this.closed) {
      this.subs.delete(sub);
      throw new Error("stream closed");
    }
    return () => this.unsubscribe(sub);
  }

  /**
   * A fresh full-screen snapshot plus terminal state (client repaints from this).
   * Epochs count *delivered* snapshots, not captures: polling captures ~4×/s,
   * so bumping on capture would make `snapshotEpoch` jump wildly between the
   * snapshots a client actually sees.
   */
  async snapshot(): Promise<StreamSnapshot> {
    const snap = await this.capture(false);
    return { ...snap, epoch: ++this.epoch };
  }

  /**
   * A snapshot that includes paged scrollback (`capture-pane -S -<lines>`).
   * The client repaints from scratch, which is how older history becomes
   * scrollable without a server-side history store.
   */
  async snapshotWithHistory(lines = MAX_HISTORY_LINES): Promise<StreamSnapshot> {
    const bounded = Math.min(MAX_HISTORY_LINES, Math.max(1, Math.floor(lines)));
    const snap = await this.capture(bounded);
    return { ...snap, epoch: ++this.epoch };
  }

  private async capture(historyLines: false | number): Promise<StreamSnapshot> {
    const state = await this.cp.snapshotState(this.sessionName);
    // A full-screen program is on the *alternate* screen, and that screen has no
    // scrollback — the history belongs to the primary screen, which is a different
    // buffer that is not on screen at all. Asking for it anyway returns the shell's
    // own prompt and history, and the client then draws that where the program's
    // screen should be. Measured: `-S -50` on an alternate-screen pane returns 70
    // lines of the *primary* screen's scrollback. So on the alternate screen what
    // is captured is the visible screen and nothing else.
    //
    // (This is also why scrolling inside vim is copy-mode rather than scrollback
    // pagination: there is no scrollback there to page.)
    const wanted = state.alternateOn ? false : historyLines;
    const raw = await this.cp.capturePane(this.sessionName, {
      escapes: true,
      history: wanted === false ? false : wanted,
      // The whole grid, not the trimmed content: a client is replaying this into
      // an emulator, and a row that stops early is a row whose background it gets
      // wrong for everything after the last character.
      pad: true,
    });
    return {
      screen: sanitizeOnce(raw),
      state,
      epoch: this.epoch,
      // Reported from what was actually captured, not from what was asked for:
      // a client told it has history scrolls into a buffer that is not there.
      ...(wanted === false ? {} : { history: true }),
    };
  }

  async inputText(text: string): Promise<void> {
    // Neither the pipe nor polling has a write channel: the pipe carries pane
    // output only (`pipe-pane` sends stdin the other way), so input is always an
    // ordinary `send-keys` in those modes.
    if (this.polling || this.pipe) {
      await this.cp.sendText(this.sessionName, text);
      return;
    }
    await this.ensureConnected();
    await this.client!.sendText(this.sessionName, text);
  }

  async sendKey(keys: string[]): Promise<void> {
    if (this.polling || this.pipe) {
      await this.cp.sendKey(this.sessionName, ...keys);
      return;
    }
    await this.ensureConnected();
    await this.client!.sendKey(this.sessionName, ...keys);
  }

  /**
   * Resize the session's window to the viewing client's grid. Deliberately not
   * routed through the control connection: the window size is a server option
   * (`window-size manual`), and every subscriber must see the same geometry.
   */
  /**
   * Enter or leave copy-mode for this session's pane.
   *
   * Deliberately goes through tmux rather than the control client's `send-keys`:
   * copy-mode is a tmux state, not bytes a program receives, so `send-keys` would
   * type the arrow keys into whatever is drawing instead of scrolling anything.
   */
  async copyMode(on: boolean): Promise<void> {
    await this.cp.copyMode(this.sessionName, on);
  }

  /** A prefix-key action, applied to this session. */
  async resize(cols: number, rows: number): Promise<void> {
    await this.cp.resizeWindow(this.sessionName, cols, rows);
  }

  /**
   * The pane's terminal state as of now (modes, cursor, size). Used to decide
   * whether an input gesture is even meaningful for the running program.
   */
  async getState(): Promise<PaneState> {
    return this.cp.snapshotState(this.sessionName);
  }

  /** Synthesize a click for a mouse-aware program in this pane. */
  async mouseClick(col: number, row: number): Promise<void> {
    await this.cp.sendMouseClick(this.sessionName, col, row);
  }

  /**
   * Switch to `capture-pane` diff polling. Used automatically when control mode
   * cannot be established; exposed so the fallback is directly testable.
   */
  startPolling(reason = "control mode unavailable"): void {
    if (this.closed || this.polling) return;
    this.polling = true;
    this.lastPolledScreen = "";
    this.lastControlAttempt = Date.now();
    this.closeClient();
    // The sanitizer buffers a partial escape sequence. Any control-mode residue
    // must not be spliced onto the diff repaint bytes polling generates.
    this.sanitizer.reset();
    for (const sub of this.subs) this.safe(() => sub.onReset(reason));
    this.schedulePoll(0);
  }

  close(reason?: string): void {
    this.closed = true;
    if (this.lingerTimer) clearTimeout(this.lingerTimer);
    if (this.pollTimer) clearTimeout(this.pollTimer);
    this.pollTimer = null;
    if (reason) for (const sub of this.subs) this.safe(() => sub.onReset(reason));
    this.subs.clear();
    this.closeClient();
    this.sanitizer.reset();
  }

  // ── internals ──────────────────────────────────────────────────────────

  private unsubscribe(sub: StreamSubscriber): void {
    this.subs.delete(sub);
    if (this.subs.size === 0 && !this.closed) {
      this.lingerTimer = setTimeout(() => this.closeClient(), LINGER_MS);
    }
  }

  private async ensureConnected(): Promise<void> {
    if (this.closed) throw new Error("stream closed");
    if (this.client || this.pipe || this.polling) return;
    if (!this.connecting) this.connecting = this.connect();
    try {
      await this.connecting;
    } finally {
      this.connecting = null;
    }
    if (this.closed) throw new Error("stream closed");
  }

  private async connect(): Promise<void> {
    if (this.opts.transport === "pipe") await this.connectPipe();
    else await this.connectControl();
    if (this.polling) {
      // A live transport recovered; stop polling to avoid double output.
      this.polling = false;
      this.lastPolledScreen = "";
      if (this.pollTimer) clearTimeout(this.pollTimer);
      this.pollTimer = null;
    }
  }

  /**
   * The pipe transport: a FIFO fed by `pipe-pane`, read with no protocol at all.
   *
   * It takes the same shape as the control client — a live stream, a fan-out,
   * and a reset for subscribers when the stream dies — so that everything above
   * this class is unchanged by which one is in use.
   */
  private async connectPipe(): Promise<void> {
    if (!this.opts.pipeDir) throw new Error("transport 'pipe' needs a pipeDir");
    const pipe = await TmuxPipeClient.connect({
      tmuxSocket: this.tmuxSocket,
      sessionName: this.sessionName,
      pipeDir: this.opts.pipeDir,
      profilePath: this.opts.profilePath,
      onOutput: (text) => this.fanout(text),
      onExit: (client) => this.onPipeExit(client),
    });
    if (this.closed) {
      try {
        pipe.close();
      } catch {
        /* ignore */
      }
      throw new Error("stream closed");
    }
    this.pipe = pipe;
    this.reconnectAttempts = 0;
    this.sanitizer.reset();
  }

  private onPipeExit(exiting?: TmuxPipeClient): void {
    // A superseded pipe's late exit must not clear a live successor.
    if (exiting && this.pipe && this.pipe !== exiting) return;
    this.pipe = null;
    if (this.closed || this.subs.size === 0) return;
    for (const sub of this.subs) this.safe(() => sub.onReset("pipe stream ended"));
    void this.reconnect();
  }

  private async connectControl(): Promise<void> {
    const client = await TmuxControlClient.connect(this.tmuxSocket, this.sessionName, {
      onOutput: (_pane, text) => this.fanout(text),
      pauseAfter: this.opts.pauseAfter,
      // So a server this attach starts is not started on tmux's defaults.
      profilePath: this.opts.profilePath,
      // A paused pane has stopped streaming and its queued output is gone, so
      // subscribers must resync from a snapshot (the client resumes the pane).
      onPaused: () => this.onPaused(),
      onExit: (client) => this.onClientExit(client),
    });
    // If we were closed while connecting, ditch the client we just created:
    // nobody can reach this stream any more, so it would leak forever.
    if (this.closed) {
      try {
        client.close();
      } catch {
        /* ignore */
      }
      throw new Error("stream closed");
    }
    this.client = client;
    this.reconnectAttempts = 0;
    // Fresh control stream: drop any partial sequence left by a prior stream or
    // by the polling fallback's generated bytes.
    this.sanitizer.reset();
  }

  private onClientExit(exiting?: TmuxControlClient): void {
    // A superseded client's late exit must not clear a live successor: that
    // would orphan a running `tmux -C` process with no owner.
    if (exiting && this.client && this.client !== exiting) return;
    this.client = null;
    if (this.closed || this.subs.size === 0) return;
    for (const sub of this.subs) this.safe(() => sub.onReset("control stream ended"));
    void this.reconnect();
  }

  private onPaused(): void {
    // tmux discards a paused pane's queued output, so subscribers must resync
    // from a snapshot. Throttled: a pause storm must not thrash every client.
    const now = Date.now();
    if (now - this.lastPauseReset < PAUSE_RESET_MIN_MS) return;
    this.lastPauseReset = now;
    for (const sub of this.subs) this.safe(() => sub.onReset("pane paused"));
  }

  private async reconnect(): Promise<void> {
    while (!this.closed && this.subs.size > 0 && !this.client && !this.pipe && !this.polling) {
      if (this.reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
        // Control mode is not coming back soon; keep the pane visible.
        this.startPolling("control mode unavailable; polling");
        return;
      }
      this.reconnectAttempts++;
      await sleep(Math.min(200 * this.reconnectAttempts, 2000));
      try {
        await this.ensureConnected();
      } catch {
        /* retry */
      }
    }
  }

  // ── polling fallback ───────────────────────────────────────────────────

  private get pollMs(): number {
    return this.opts.pollMs ?? DEFAULT_POLL_MS;
  }

  private schedulePoll(delay: number): void {
    if (this.closed || !this.polling) return;
    this.pollTimer = setTimeout(() => void this.pollOnce(), delay);
  }

  private async pollOnce(): Promise<void> {
    this.pollTimer = null;
    if (this.closed || !this.polling) return;
    if (this.subs.size === 0) {
      // Nobody is watching: stop polling entirely. The linger timer closes the
      // stream; a later subscriber reconnects (and re-enters polling if needed).
      this.polling = false;
      this.lastPolledScreen = "";
      return;
    }
    try {
      const snap = await this.capture(false);
      if (this.lastPolledScreen === "") {
        // Seed from the first capture without emitting: subscribers already
        // hold a freshly attached snapshot, so re-emitting the whole screen
        // would duplicate it.
        this.lastPolledScreen = snap.screen;
      } else {
        const update = screenUpdate(this.lastPolledScreen, snap.screen);
        this.lastPolledScreen = snap.screen;
        if (update.kind !== "none") this.fanout(update.text);
      }
    } catch {
      // The session may be gone; the daemon's stream sweep decides that.
    }
    // Periodically give control mode another chance (it is strictly better).
    const now = Date.now();
    if (now - this.lastControlAttempt > CONTROL_RETRY_MS) {
      this.lastControlAttempt = now;
      if (await this.tryControlRecovery()) return;
    }
    this.schedulePoll(this.pollMs);
  }

  /**
   * Leave polling long enough to test whether control mode works again.
   * Returns true when a control client is live (which stops polling).
   */
  private async tryControlRecovery(): Promise<boolean> {
    if (this.client) return true;
    this.polling = false;
    try {
      await this.ensureConnected();
      return this.client !== null;
    } catch {
      this.polling = true;
      return false;
    }
  }

  private fanout(text: string): void {
    if (this.closed) return;
    // Filter clipboard/DCS/device-status before any client sees it (ADR-0001).
    const clean = this.sanitizer.push(text);
    if (!clean) return;
    for (const sub of this.subs) this.safe(() => sub.onOutput(clean));
  }

  private safe(fn: () => void): void {
    try {
      fn();
    } catch {
      /* a faulty subscriber must not break the stream */
    }
  }

  private closeClient(): void {
    const client = this.client;
    this.client = null;
    try {
      client?.close();
    } catch {
      /* ignore */
    }
    const pipe = this.pipe;
    this.pipe = null;
    try {
      pipe?.close();
    } catch {
      /* ignore */
    }
  }
}