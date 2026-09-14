/**
 * Remote WebSocket transport.
 *
 * Binds loopback only (ADR-0005): the intended exposure is
 *   tailscale serve --bg --https=443 http://127.0.0.1:<port>
 * so TLS + tailnet identity are provided by Tailscale, and this server adds a
 * bearer-token gate. Terminal output is sent as binary OUTPUT frames (frames.ts
 * — the one canonical frame format); control/handshake is JSON text.
 *
 * One connection = one attached session (open N connections to view N sessions).
 * On attach: subscribe first (queueing), snapshot, send CAPS+SNAPSHOT, then
 * flush — so no output is lost across the snapshot boundary.
 */

import { encodeFrame, FrameType } from "../../protocol/src/frames.ts";
import {
  PROTOCOL_VERSION,
  type Caps,
  type SessionInfo,
  type Snapshot,
} from "../../protocol/src/messages.ts";
import type { SessionRecord } from "../../protocol/src/rpc.ts";
import { tokenMatches } from "./auth.ts";
import type { PaneStream } from "./stream.ts";
import { extname, join, normalize } from "node:path";
import { realpath } from "node:fs/promises";

export interface RemoteServerOptions {
  hostname: string;
  /** 0 = ephemeral (tests). */
  port: number;
  token: string;
  getSession(id: string): SessionRecord | undefined;
  listSessions(): SessionInfo[];
  acquireStream(record: SessionRecord): PaneStream;
  /** Called when a stream is no longer used (failed attach / closed socket). */
  releaseStream?(record: SessionRecord): void;
  /** Directory of the static web client to serve at /. */
  webRoot?: string;
  maxBufferedBytes?: number;
  log?: (msg: string) => void;
}

interface ConnData {
  authed: boolean;
  disposed: boolean;
  sessionId?: string;
  stream?: PaneStream;
  unsubscribe?: () => void;
  live: boolean;
  pending: string[];
  epoch: number;
}

const DEFAULT_MAX_BUFFERED = 4 * 1024 * 1024;

export class RemoteServer {
  private server: any;
  private readonly encoder = new TextEncoder();

  constructor(private readonly opts: RemoteServerOptions) {}

  get port(): number {
    return this.server?.port ?? this.opts.port;
  }

  start(): { hostname: string; port: number } {
    const opts = this.opts;
    this.server = Bun.serve({
      hostname: opts.hostname,
      port: opts.port,
      fetch: (req: Request, server: any) => this.handleFetch(req, server),
      websocket: {
        open: () => {
          /* wait for attach */
        },
        message: (ws: any, raw: any) => void this.onMessage(ws, raw),
        close: (ws: any) => {
          const data = ws.data as ConnData;
          data.disposed = true;
          this.cleanupSubscription(data);
        },
        drain: () => {
          /* backpressure hook (unused) */
        },
      },
      error: (err: unknown) => {
        opts.log?.(`remote: server error: ${(err as Error).message}`);
        return new Response("error\n", { status: 500 });
      },
    });
    opts.log?.(`remote: ws://${opts.hostname}:${this.port}/ws`);
    return { hostname: opts.hostname, port: this.port };
  }

  stop(): void {
    try {
      this.server?.stop(true);
    } catch {
      /* ignore */
    }
  }

  private async handleFetch(req: Request, server: any): Promise<Response | undefined> {
    const url = new URL(req.url);
    if (url.pathname !== "/ws") return this.serveStatic(url.pathname);
    const auth = req.headers.get("authorization") ?? "";
    const provided = auth.startsWith("Bearer ")
      ? auth.slice("Bearer ".length).trim()
      : (url.searchParams.get("token") ?? "");
    if (!tokenMatches(this.opts.token, provided)) return new Response("unauthorized\n", { status: 401 });
    const data: ConnData = { authed: true, disposed: false, live: false, pending: [], epoch: 0 };
    if (server.upgrade(req, { data })) return undefined;
    return new Response("upgrade failed\n", { status: 400 });
  }

  /** Serve the bundled web client (buildless static assets). */
  private async serveStatic(pathname: string): Promise<Response> {
    const root = this.opts.webRoot;
    if (!root) return new Response("ldmux remote\n", { status: 200 });
    const rel = pathname === "/" ? "/index.html" : pathname;
    const full = normalize(join(root, rel));
    if (full !== root && !full.startsWith(root + "/")) return new Response("forbidden\n", { status: 403 });
    // Confine again after symlink resolution (Bun.file follows links).
    const realRoot = await realpath(root).catch(() => root);
    const real = await realpath(full).catch(() => null);
    if (!real) return new Response("not found\n", { status: 404 });
    if (real !== realRoot && !real.startsWith(realRoot + "/")) return new Response("forbidden\n", { status: 403 });
    const file = Bun.file(real);
    return new Response(file, { headers: { "content-type": contentType(real) } });
  }

  private async onMessage(ws: any, raw: any): Promise<void> {
    const data = ws.data as ConnData;
    if (!data.authed) {
      ws.close();
      return;
    }
    let msg: any;
    try {
      msg = JSON.parse(typeof raw === "string" ? raw : new TextDecoder().decode(raw));
    } catch {
      this.sendError(ws, "bad_json", "invalid JSON");
      return;
    }
    try {
      switch (msg.type) {
        case "ping":
          this.sendJson(ws, { type: "pong", id: msg.id });
          return;
        case "list":
          this.sendJson(ws, { type: "sessions", sessions: this.opts.listSessions() });
          return;
        case "attach":
          await this.attach(ws, data, String(msg.sessionId ?? ""));
          return;
        case "input":
          if (data.stream) await data.stream.inputText(String(msg.data ?? ""));
          return;
        case "key":
          if (data.stream) {
            const keys = (Array.isArray(msg.keys) ? msg.keys : []).filter((k: unknown): k is string => typeof k === "string");
            await data.stream.sendKey(keys);
          }
          return;
        case "follow":
          return; // client-owned scroll (ADR-0004)
        default:
          this.sendError(ws, "bad_message", `unknown type: ${String(msg.type)}`);
      }
    } catch (err) {
      this.sendError(ws, "handler_error", (err as Error).message);
    }
  }

  private async attach(ws: any, data: ConnData, sessionId: string): Promise<void> {
    const record = this.opts.getSession(sessionId);
    if (!record || !record.tmuxName) {
      this.sendError(ws, "unknown_session", `no terminal-backed session: ${sessionId}`);
      return;
    }
    if (record.status !== "running") {
      this.sendError(ws, "session_dead", `session is ${record.status}: ${sessionId}`);
      return;
    }
    this.cleanupSubscription(data);
    data.sessionId = sessionId;
    data.live = false;
    data.pending = [];
    const stream = this.opts.acquireStream(record);
    data.stream = stream;

    let unsubscribe: (() => void) | undefined;
    try {
      unsubscribe = await stream.subscribe({
        onOutput: (text) => {
          if (data.live) this.sendOutput(ws, text);
          else data.pending.push(text);
        },
        onReset: (reason) => this.sendError(ws, "resync", reason),
      });
    } catch (err) {
      data.stream = undefined;
      this.opts.releaseStream?.(record);
      this.sendError(ws, "attach_failed", (err as Error).message);
      return;
    }
    // The client may have disconnected while we awaited subscribe(): commit
    // nothing, or we would resurrect a subscription on a dead socket.
    if (data.disposed) {
      unsubscribe();
      this.opts.releaseStream?.(record);
      return;
    }
    data.unsubscribe = unsubscribe;

    const snap = stream.snapshot();
    data.epoch = snap.epoch;
    const caps: Caps = {
      type: "caps",
      protocolVersion: PROTOCOL_VERSION,
      sessionId,
      cols: snap.state.cols,
      rows: snap.state.rows,
      snapshotEpoch: snap.epoch,
      capabilities: { controlMode: true, scrollback: true },
    };
    const snapshot: Snapshot = {
      type: "snapshot",
      sessionId,
      cols: snap.state.cols,
      rows: snap.state.rows,
      screen: snap.screen,
      cursorX: snap.state.cursorX,
      cursorY: snap.state.cursorY,
      alternateOn: snap.state.alternateOn,
      paneInMode: snap.state.paneInMode,
      bracketedPaste: false,
      mouse: false,
      appCursor: false,
      historySize: snap.state.historySize,
      snapshotEpoch: snap.epoch,
    };
    this.sendJson(ws, caps);
    this.sendJson(ws, snapshot);

    data.live = true;
    const pending = data.pending;
    data.pending = [];
    for (const text of pending) this.sendOutput(ws, text);
  }

  private sendJson(ws: any, msg: unknown): void {
    try {
      ws.send(JSON.stringify(msg));
    } catch {
      /* client gone */
    }
  }

  private sendError(ws: any, code: string, message: string): void {
    this.sendJson(ws, { type: "error", code, message });
  }

  private sendOutput(ws: any, text: string): void {
    let frame: Uint8Array;
    try {
      frame = encodeFrame(FrameType.Output, this.encoder.encode(text));
    } catch {
      // Oversized payload (MAX_FRAME_PAYLOAD): force a snapshot resync instead
      // of silently dropping output.
      this.sendError(ws, "resync", "output frame too large");
      return;
    }
    try {
      ws.send(frame);
    } catch {
      /* client gone */
      return;
    }
    const buffered = typeof ws.getBufferedAmount === "function" ? ws.getBufferedAmount() : 0;
    if (buffered > (this.opts.maxBufferedBytes ?? DEFAULT_MAX_BUFFERED)) {
      this.sendError(ws, "too_far_behind", "client lagging; reconnect for a fresh snapshot");
      try {
        ws.close();
      } catch {
        /* ignore */
      }
    }
  }

  private cleanupSubscription(data: ConnData): void {
    try {
      data.unsubscribe?.();
    } catch {
      /* ignore */
    }
    data.unsubscribe = undefined;
    data.stream = undefined;
  }
}

const CONTENT_TYPES: Record<string, string> = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".mjs": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".map": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".png": "image/png",
  ".ico": "image/x-icon",
  ".txt": "text/plain; charset=utf-8",
};

function contentType(path: string): string {
  return CONTENT_TYPES[extname(path).toLowerCase()] ?? "application/octet-stream";
}