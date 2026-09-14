/**
 * PaneStream: one live tmux control-mode client per viewed session, fanned out
 * to any number of subscribers (WebSocket clients).
 *
 * Lifecycle: the control client is created on the first subscriber, survives
 * brief subscriber churn (5 s linger), and is torn down when truly idle. If the
 * control stream dies, subscribers are told to resync (`onReset`) and the
 * client is transparently reconnected with bounded backoff. Snapshot-on-connect
 * is the correctness backstop (ADR-0001/0004): live output is best-effort.
 */

import type { PaneState } from "../../protocol/src/rpc.ts";
import { TmuxControlClient } from "./tmux/control-client.ts";
import type { TmuxControlPlane } from "./tmux/control-plane.ts";

export interface StreamSubscriber {
  onOutput(text: string): void;
  onReset(reason: string): void;
}

export interface StreamSnapshot {
  screen: string;
  state: PaneState;
  epoch: number;
}

const LINGER_MS = 5000;
const MAX_RECONNECT_ATTEMPTS = 10;

export class PaneStream {
  private client: TmuxControlClient | null = null;
  private connecting: Promise<void> | null = null;
  private readonly subs = new Set<StreamSubscriber>();
  private lingerTimer: ReturnType<typeof setTimeout> | null = null;
  private closed = false;
  private epoch = 0;
  private reconnectAttempts = 0;

  constructor(
    private readonly tmuxSocket: string,
    readonly sessionName: string,
    private readonly cp: TmuxControlPlane,
  ) {}

  get subscriberCount(): number {
    return this.subs.size;
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
    return () => this.unsubscribe(sub);
  }

  /** A fresh full-screen snapshot plus terminal state (client repaints from this). */
  snapshot(): StreamSnapshot {
    return {
      screen: this.cp.capturePane(this.sessionName, { escapes: true }),
      state: this.cp.snapshotState(this.sessionName),
      epoch: ++this.epoch,
    };
  }

  async inputText(text: string): Promise<void> {
    await this.ensureConnected();
    await this.client!.sendText(this.sessionName, text);
  }

  async sendKey(keys: string[]): Promise<void> {
    await this.ensureConnected();
    await this.client!.sendKey(this.sessionName, ...keys);
  }

  close(reason?: string): void {
    this.closed = true;
    if (this.lingerTimer) clearTimeout(this.lingerTimer);
    if (reason) for (const sub of this.subs) this.safe(() => sub.onReset(reason));
    this.subs.clear();
    this.closeClient();
  }

  // ── internals ──────────────────────────────────────────────────────────

  private unsubscribe(sub: StreamSubscriber): void {
    this.subs.delete(sub);
    if (this.subs.size === 0 && !this.closed) {
      this.lingerTimer = setTimeout(() => this.closeClient(), LINGER_MS);
    }
  }

  private async ensureConnected(): Promise<void> {
    if (this.client) return;
    if (!this.connecting) this.connecting = this.connect();
    try {
      await this.connecting;
    } finally {
      this.connecting = null;
    }
  }

  private async connect(): Promise<void> {
    const client = await TmuxControlClient.connect(this.tmuxSocket, this.sessionName, {
      onOutput: (_pane, text) => this.fanout(text),
      onExit: () => this.onClientExit(),
    });
    this.client = client;
    this.reconnectAttempts = 0;
  }

  private onClientExit(): void {
    this.client = null;
    if (this.closed || this.subs.size === 0) return;
    for (const sub of this.subs) this.safe(() => sub.onReset("control stream ended"));
    void this.reconnect();
  }

  private async reconnect(): Promise<void> {
    while (!this.closed && this.subs.size > 0 && !this.client) {
      if (this.reconnectAttempts >= MAX_RECONNECT_ATTEMPTS) {
        for (const sub of this.subs) this.safe(() => sub.onReset("reconnect exhausted"));
        return;
      }
      this.reconnectAttempts++;
      await Bun.sleep(Math.min(200 * this.reconnectAttempts, 2000));
      try {
        await this.ensureConnected();
      } catch {
        /* retry */
      }
    }
  }

  private fanout(text: string): void {
    if (this.closed) return;
    for (const sub of this.subs) this.safe(() => sub.onOutput(text));
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
  }
}