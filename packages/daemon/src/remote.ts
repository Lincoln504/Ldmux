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
  type Attach,
  type Caps,
  type ControlMessage,
  type SessionInfo,
  type Snapshot,
} from "../../protocol/src/messages.ts";
import type { SessionRecord } from "../../protocol/src/rpc.ts";
import { tokenMatches } from "./auth.ts";
import type { PaneStream, StreamSnapshot } from "./stream.ts";
import { extname, join, normalize } from "node:path";
import { realpath } from "node:fs/promises";

export interface RemoteServerOptions {
  hostname: string;
  /** 0 = ephemeral (tests). */
  port: number;
  token: string;
  getSession(id: string): SessionRecord | undefined;
  listSessions(): SessionInfo[] | Promise<SessionInfo[]>;
  /** Open a new session on the host (FR-10). */
  createSession?(kind: "shell" | "pi", cwd: string | undefined, title: string | undefined): Promise<void>;
  /** End a session by id. */
  killSession?(id: string): Promise<void>;
  acquireStream(record: SessionRecord): PaneStream;
  /** Called when a stream is no longer used (failed attach / closed socket). */
  releaseStream?(record: SessionRecord): void;
  /** Directory of the static web client to serve at /. */
  webRoot?: string;
  maxBufferedBytes?: number;
  log?: (msg: string) => void;
}

interface ConnData {
  disposed: boolean;
  /** Incremented per attach; a superseded attach must not commit. */
  attachGen: number;
  sessionId?: string;
  stream?: PaneStream;
  /** The record of the attached session, so the close path can release it. */
  record?: SessionRecord;
  unsubscribe?: () => void;
  live: boolean;
  pending: string[];
  pendingBytes: number;
}

const DEFAULT_MAX_BUFFERED = 4 * 1024 * 1024;
/** Per-type caps adapted from dooboxes' codec design (TERMINAL_CODEC_DESIGN.md §5). */
const MAX_INPUT_BYTES = 16 * 1024;
const MAX_KEYS = 32;
/** Output queued between subscribe and snapshot is bounded before we resync. */
const MAX_PENDING_BYTES = 1024 * 1024;

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
        // Bun closes on a bigger frame (1009) before our handler can answer;
        // 64 KiB leaves room for the JSON envelope around a 16 KiB payload,
        // which is where the graceful `input_too_large` check fires.
        maxPayloadLength: MAX_INPUT_BYTES * 4,
        open: () => {
          /* wait for attach */
        },
        message: (ws: any, raw: any) => void this.onMessage(ws, raw),
        close: (ws: any) => {
          const data = ws.data as ConnData;
          data.disposed = true;
          this.cleanupSubscription(data);
          // Consistent with the failed/superseded attach paths: a stream with no
          // remaining subscribers is dropped rather than cached for the daemon's
          // lifetime. `releaseStreamIfIdle` is a no-op if another client attached.
          if (data.record) this.opts.releaseStream?.(data.record);
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
    const data: ConnData = {
      disposed: false,
      attachGen: 0,
      live: false,
      pending: [],
      pendingBytes: 0,
    };
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
    // Typed as the protocol union: the server and the contract cannot drift,
    // and every member of the union has a sender (there is no dead wire surface).
    let msg: ControlMessage;
    try {
      msg = JSON.parse(typeof raw === "string" ? raw : new TextDecoder().decode(raw)) as ControlMessage;
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
          await this.sendSessions(ws);
          return;
        case "new": {
          await this.opts.createSession?.(msg.kind === "pi" ? "pi" : "shell", msg.cwd, msg.title);
          await this.sendSessions(ws);
          return;
        }
        case "kill": {
          await this.opts.killSession?.(String(msg.sessionId ?? ""));
          await this.sendSessions(ws);
          return;
        }
        case "attach":
          await this.attach(ws, data, String(msg.sessionId ?? ""), msg);
          return;
        case "resize": {
          if (!data.stream || !data.live || !data.record) {
            this.sendError(ws, "not_attached", "attach before resizing");
            return;
          }
          const wantCols = Math.trunc(Number(msg.cols));
          const wantRows = Math.trunc(Number(msg.rows));
          if (!Number.isFinite(wantCols) || !Number.isFinite(wantRows)) {
            this.sendError(ws, "bad_size", `invalid size: ${String(msg.cols)}x${String(msg.rows)}`);
            return;
          }
          const gen = data.attachGen;
          try {
            await data.stream.resize(wantCols, wantRows);
          } catch (err) {
            this.sendError(ws, "resize_failed", (err as Error).message);
            return;
          }
          // The pane geometry changed, so every pixel the client holds is stale:
          // answer with a fresh snapshot rather than a diff against a different
          // grid.
          const resized = await data.stream.snapshot();
          if (data.disposed || data.attachGen !== gen) return;
          this.sendState(ws, String(data.sessionId ?? ""), resized);
          return;
        }
        case "input": {
          if (data.stream) {
            const text = String(msg.data ?? "");
            // Measured in bytes, not UTF-16 code units: `length` would let a
            // 16 Ki-code-unit string of 4-byte characters through as ~64 KiB and
            // be closed at the transport instead of refused gracefully.
            if (this.encoder.encode(text).length > MAX_INPUT_BYTES) {
              this.sendError(ws, "input_too_large", `input exceeds ${MAX_INPUT_BYTES} bytes`);
              return;
            }
            await data.stream.inputText(text);
          }
          return;
        }
        case "key":
          if (data.stream) {
            const keys = (Array.isArray(msg.keys) ? msg.keys : [])
              // strings only, and never a leading dash: tmux would read it as a flag.
              .filter((k: unknown): k is string => typeof k === "string" && k.length > 0 && !k.startsWith("-"))
              .slice(0, MAX_KEYS);
            await data.stream.sendKey(keys);
          }
          return;
        case "follow":
          return; // client-owned scroll (ADR-0004)
        case "scrollback": {
          if (data.stream) {
            const gen = data.attachGen;
            const lines = Number(msg.lines ?? 0);
            const snap = await data.stream.snapshotWithHistory(
              Number.isFinite(lines) && lines > 0 ? lines : undefined,
            );
            // Re-check after the await: a newer attach (or a disconnect) may have
            // superseded this request, and repainting another session's screen
            // into the new one is worse than sending nothing.
            if (data.disposed || data.attachGen !== gen) return;
            // The client repaints from scratch, so this is a Snapshot carrying
            // the `history` flag rather than a separate message shape.
            this.sendState(ws, String(data.sessionId ?? ""), snap, { history: true });
          } else {
            this.sendError(ws, "not_attached", "attach before requesting scrollback");
          }
          return;
        }
        default:
          this.sendError(ws, "bad_message", `unknown type: ${String(msg.type)}`);
      }
    } catch (err) {
      this.sendError(ws, "handler_error", (err as Error).message);
    }
  }

  private async attach(ws: any, data: ConnData, sessionId: string, msg: Attach): Promise<void> {
    const record = this.opts.getSession(sessionId);
    if (!record || !record.tmuxName) {
      this.sendError(ws, "unknown_session", `no terminal-backed session: ${sessionId}`);
      return;
    }
    if (record.status !== "running") {
      this.sendError(ws, "session_dead", `session is ${record.status}: ${sessionId}`);
      return;
    }
    const gen = ++data.attachGen;
    this.cleanupSubscription(data);
    data.sessionId = sessionId;
    data.record = record;
    data.live = false;
    data.pending = [];
    data.pendingBytes = 0;
    const stream = this.opts.acquireStream(record);
    data.stream = stream;

    let unsubscribe: (() => void) | undefined;
    try {
      unsubscribe = await stream.subscribe({
        onOutput: (text) => {
          if (data.live) this.sendOutput(ws, text);
          else {
            data.pendingBytes += text.length;
            if (data.pendingBytes > MAX_PENDING_BYTES) {
              // Fell too far behind while snapshotting: resync instead of growing.
              data.pending = [];
              data.pendingBytes = 0;
              this.sendError(ws, "resync", "output backlog exceeded while attaching");
            } else {
              data.pending.push(text);
            }
          }
        },
        onReset: (reason) => {
          if (data.attachGen === gen) this.sendError(ws, "resync", reason);
        },
      });
    } catch (err) {
      this.opts.releaseStream?.(record);
      if (data.attachGen === gen && !data.disposed) {
        data.stream = undefined;
        this.sendError(ws, "attach_failed", (err as Error).message);
      }
      return;
    }
    // A second attach (or a disconnect) may have superseded us while we awaited:
    // commit nothing, or we would leak this subscription and mix two panes.
    if (data.disposed || data.attachGen !== gen) {
      unsubscribe();
      this.opts.releaseStream?.(record);
      return;
    }
    data.unsubscribe = unsubscribe;

    // The viewing client owns the geometry (a phone's viewport is the authority
    // on its own screen size). Resize *before* the snapshot so the screen we
    // send already matches the grid the client is about to paint, instead of
    // sending 120x40 and letting it reflow one frame later.
    const wantCols = Math.trunc(Number(msg.cols));
    const wantRows = Math.trunc(Number(msg.rows));
    if (Number.isFinite(wantCols) && Number.isFinite(wantRows) && wantCols > 0 && wantRows > 0) {
      try {
        await stream.resize(wantCols, wantRows);
      } catch (err) {
        // Not fatal: an unsized pane still beats no pane at all.
        this.opts.log?.(`remote: resize on attach failed: ${(err as Error).message}`);
      }
    }

    const snap = await stream.snapshot();
    // Re-check after the await: the socket may have gone or a newer attach won.
    if (data.disposed || data.attachGen !== gen) {
      this.opts.releaseStream?.(record);
      return;
    }
    this.sendState(ws, sessionId, snap);

    data.live = true;
    const pending = data.pending;
    data.pending = [];
    data.pendingBytes = 0;
    for (const text of pending) this.sendOutput(ws, text);
  }

  private sendJson(ws: any, msg: unknown): void {
    try {
      ws.send(JSON.stringify(msg));
    } catch {
      /* client gone */
    }
  }

  /**
   * CAPS + SNAPSHOT for one capture. The pair is always sent together (the
   * client reads the size from both) and is the answer to attach, resize and
   * scrollback, so the three cannot drift apart.
   */
  private sendState(ws: any, sessionId: string, snap: StreamSnapshot, opts: { history?: boolean } = {}): void {
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
      bracketedPaste: snap.state.bracketedPaste,
      mouse: snap.state.mouse,
      appCursor: snap.state.appCursor,
      historySize: snap.state.historySize,
      snapshotEpoch: snap.epoch,
      ...(opts.history ? { history: true } : {}),
    };
    this.sendJson(ws, caps);
    this.sendJson(ws, snapshot);
  }

  /** Send the current session list (the answer to list/new/kill). */
  private async sendSessions(ws: any): Promise<void> {
    this.sendJson(ws, { type: "sessions", sessions: await this.opts.listSessions() });
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
  ".webmanifest": "application/manifest+json; charset=utf-8",
};

function contentType(path: string): string {
  return CONTENT_TYPES[extname(path).toLowerCase()] ?? "application/octet-stream";
}