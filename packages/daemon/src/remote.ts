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
import { isShellForeground } from "./desktop.ts";
import type { PaneStream, StreamSnapshot } from "./stream.ts";
import { extname, join, normalize } from "node:path";
import { readFile, realpath } from "node:fs/promises";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { WebSocketServer, type WebSocket } from "ws";

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
  /** Output waiting for the coalescing window (see [RemoteServer.queueOutput]). */
  outBuf: string[];
  outBytes: number;
  flushTimer: ReturnType<typeof setTimeout> | null;
  /**
   * Serializes control-message handling for this connection.
   *
   * Handlers await tmux, so two messages that arrive in the same tick would
   * otherwise interleave — and for `attach` that is not merely untidy: the second
   * attach invalidates the first mid-flight, so which snapshot a client receives
   * would depend on the timing of the transport. Chaining makes the order the
   * client sent them the order they take effect (the Unix-socket path already
   * works this way).
   */
  queue: Promise<void>;
}

const DEFAULT_MAX_BUFFERED = 4 * 1024 * 1024;
/** Per-type caps adapted from dooboxes' codec design (TERMINAL_CODEC_DESIGN.md §5). */
const MAX_INPUT_BYTES = 16 * 1024;
const MAX_KEYS = 32;
/** Output queued between subscribe and snapshot is bounded before we resync. */
const MAX_PENDING_BYTES = 1024 * 1024;
/**
 * Output coalescing. The pane is fed by tmux in many small writes; sending each
 * as its own WebSocket frame costs a packet header, a wakeup and a render on the
 * phone for what is usually one visible line. A short window merges a burst into
 * one frame instead, which is both lighter on the network and far cheaper for the
 * client. It is deliberately small: it is a batching window, not a frame rate,
 * and it must stay under the threshold where a keystroke echo feels delayed.
 */
const OUTPUT_FLUSH_MS = 8;
const OUTPUT_FLUSH_BYTES = 16 * 1024;

export class RemoteServer {
  private server: Server | undefined;
  private wss: WebSocketServer | undefined;
  /**
   * Per-connection state. `ws` has no place to hang it on the socket, so it
   * lives here, keyed by the socket itself.
   */
  private readonly connState = new WeakMap<object, ConnData>();
  private readonly encoder = new TextEncoder();

  private readonly opts: RemoteServerOptions;

  constructor(opts: RemoteServerOptions) {
    this.opts = opts;
  }

  get port(): number {
    const addr = this.server?.address();
    if (addr && typeof addr === "object") return addr.port;
    return this.opts.port;
  }

  /**
   * Bind and begin serving. Resolves once the socket is actually listening, so
   * the returned port is real — with port 0 (tests, ephemeral) the caller cannot
   * know it any other way.
   */
  async start(): Promise<{ hostname: string; port: number }> {
    const opts = this.opts;
    this.server = createServer((req, res) => {
      // Never leave this promise unobserved: an unhandled rejection here is a
      // dead daemon, and every attached terminal with it.
      this.handleRequest(req, res).catch((err: Error) => {
        this.opts.log?.(`remote: request failed: ${err.message}`);
        try {
          if (!res.headersSent) res.writeHead(500, { "content-type": "text/plain" });
          res.end("error\n");
        } catch {
          /* the socket is already gone */
        }
      });
    });
    // The transport closes on an oversized frame (1009) before our handler can
    // answer; a 64 KiB limit leaves room for the JSON envelope around a 16 KiB
    // payload, which is where the graceful `input_too_large` check fires.
    this.wss = new WebSocketServer({ noServer: true, maxPayload: MAX_INPUT_BYTES * 4 });
    this.wss.on("connection", (socket: WebSocket) => {
      const data = this.connState.get(socket);
      if (!data) {
        socket.close();
        return;
      }
      socket.on("message", (raw: Buffer | ArrayBuffer | Buffer[], isBinary: boolean) => {
        const msg = normaliseIncoming(raw, isBinary);
        data.queue = data.queue
          .then(() => this.onMessage(socket, msg))
          .catch((err: Error) => {
            this.sendError(socket, "internal", err.message);
          });
      });
      socket.on("close", () => {
        const conn = this.connState.get(socket);
        if (!conn) return;
        conn.disposed = true;
        if (conn.flushTimer) {
          clearTimeout(conn.flushTimer);
          conn.flushTimer = null;
        }
        this.cleanupSubscription(conn);
        // Consistent with the failed/superseded attach paths: a stream with no
        // remaining subscribers is dropped rather than cached for the daemon's
        // lifetime. `releaseStreamIfIdle` is a no-op if another client attached.
        if (conn.record) this.opts.releaseStream?.(conn.record);
      });
      socket.on("error", () => {
        /* a dropped client is not an event */
      });
    });
    this.server.on("upgrade", (req: IncomingMessage, socket, head) => {
      if (!this.authorized(req)) {
        socket.write("HTTP/1.1 401 Unauthorized\r\nConnection: close\r\n\r\n");
        socket.destroy();
        return;
      }
      const data: ConnData = {
        disposed: false,
        attachGen: 0,
        live: false,
        pending: [],
        pendingBytes: 0,
        outBuf: [],
        outBytes: 0,
        flushTimer: null,
        queue: Promise.resolve(),
      };
      this.wss!.handleUpgrade(req, socket, head, (client: WebSocket) => {
        this.connState.set(client, data);
        this.wss!.emit("connection", client);
      });
    });
    this.server.on("error", (err: Error) => {
      opts.log?.(`remote: server error: ${err.message}`);
    });
    await new Promise<void>((resolve, reject) => {
      const onError = (err: Error) => reject(err);
      this.server!.once("error", onError);
      this.server!.listen(opts.port, opts.hostname, () => {
        this.server!.off("error", onError);
        resolve();
      });
    });
    opts.log?.(`remote: ws://${opts.hostname}:${this.port}/ws`);
    return { hostname: opts.hostname, port: this.port };
  }

  stop(): void {
    try {
      for (const client of this.wss?.clients ?? []) client.terminate();
      this.wss?.close();
    } catch {
      /* ignore */
    }
    try {
      this.server?.close();
    } catch {
      /* ignore */
    }
  }

  /**
   * Parse the request target defensively.
   *
   * `req.url` and `Host` are both attacker-controlled, and the WHATWG parser
   * *throws* on some of what an HTTP client may legally send — an absolute-form
   * target with no authority (`GET http:// HTTP/1.1`) is accepted by Node's HTTP
   * parser and reaches us verbatim. This runs before any token check, so an
   * unguarded `new URL` here is an unauthenticated way to take the daemon down.
   */
  private parseUrl(req: IncomingMessage): URL | null {
    try {
      return new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
    } catch {
      return null;
    }
  }

  /** Is this upgrade carrying the bearer token (header or query)? */
  private authorized(req: IncomingMessage): boolean {
    const url = this.parseUrl(req);
    if (!url) return false;
    const header = req.headers.authorization ?? "";
    const provided = header.startsWith("Bearer ")
      ? header.slice("Bearer ".length).trim()
      : (url.searchParams.get("token") ?? "");
    return tokenMatches(this.opts.token, provided);
  }

  private async handleRequest(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const url = this.parseUrl(req);
    if (!url) {
      res.writeHead(400, { "content-type": "text/plain; charset=utf-8" });
      res.end("bad request\n");
      return;
    }
    // /ws is handled by the upgrade event; a plain GET there is a mistake, but
    // an unauthenticated one is still a 401 — the token gate must not be
    // bypassable by declining to upgrade.
    if (url.pathname === "/ws") {
      if (!this.authorized(req)) {
        res.writeHead(401, { "content-type": "text/plain; charset=utf-8" });
        res.end("unauthorized\n");
        return;
      }
      res.writeHead(400, { "content-type": "text/plain; charset=utf-8" });
      res.end("upgrade required\n");
      return;
    }
    const staticRes = await this.serveStatic(url.pathname);
    res.writeHead(staticRes.status, { "content-type": staticRes.headers.get("content-type") ?? "text/plain" });
    res.end(Buffer.from(await staticRes.arrayBuffer()));
  }

  /** Serve the bundled web client (buildless static assets). */
  private async serveStatic(pathname: string): Promise<Response> {
    const text = (status: number, body: string) =>
      new Response(body, { status, headers: { "content-type": "text/plain; charset=utf-8" } });
    const root = this.opts.webRoot;
    if (!root) return new Response("ldmux remote\n", { status: 200 });
    const rel = pathname === "/" ? "/index.html" : pathname;
    const full = normalize(join(root, rel));
    // `normalize` strips a trailing slash from the root, so one comparison covers
    // both `/srv/web` and `/srv/web/` (and a root of `/`).
    const base = normalize(root);
    if (full !== base && !full.startsWith(base + "/")) return text(403, "forbidden\n");
    // Confine again after symlink resolution: a symlink inside the web root must
    // not become a way to read the rest of the filesystem.
    const realRoot = normalize(await realpath(base).catch(() => base));
    const real = await realpath(full).catch(() => null);
    if (!real) return text(404, "not found\n");
    if (real !== realRoot && !real.startsWith(realRoot + "/")) return text(403, "forbidden\n");
    const body = await readFile(real).catch(() => null);
    if (!body) return text(404, "not found\n");
    return new Response(body, { headers: { "content-type": contentType(real) } });
  }

  private async onMessage(ws: any, raw: any): Promise<void> {
    const data = this.connState.get(ws);
    if (!data) return;
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
          if (data.disposed) return;
          await this.sendSessions(ws);
          return;
        }
        case "kill": {
          await this.opts.killSession?.(String(msg.sessionId ?? ""));
          if (data.disposed) return;
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
          // Anything buffered must land before the repaint it predates.
          this.flushOutput(ws, data);
          this.sendState(ws, String(data.sessionId ?? ""), resized);
          return;
        }
        case "mouseclick": {
          if (!data.stream) {
            this.sendError(ws, "not_attached", "attach before clicking");
            return;
          }
          const col = Math.trunc(Number(msg.col));
          const row = Math.trunc(Number(msg.row));
          if (!Number.isFinite(col) || !Number.isFinite(row) || col < 0 || row < 0) {
            this.sendError(ws, "bad_cell", `invalid cell: ${String(msg.col)},${String(msg.row)}`);
            return;
          }
          // Only for a pane whose program actually asked for mouse reporting;
          // anything else would be fed an escape sequence it will print as junk.
          // Deliberately not `getState?.()`: the method is required, and failing
          // open here would defeat the guard it exists to provide.
          const state = await data.stream.getState();
          if (!(state.mouse && state.mouseSgr)) {
            this.sendError(ws, "no_mouse", "pane has not enabled SGR mouse reporting");
            return;
          }
          // The mode is sticky: a program that enabled reporting and then exited
          // leaves it on, so the flags above still say "mouse" while a shell is
          // what would receive the bytes. A shell prints them as junk
          // (`^[[<0;42;8M`), so refuse — and the client answers this by moving
          // the cursor instead, which is what the tap meant.
          if (isShellForeground(state.foreground)) {
            this.sendError(
              ws,
              "shell_foreground",
              `pane is at a shell prompt (${state.foreground}), not a mouse-aware program`,
            );
            return;
          }
          if (col >= state.cols || row >= state.rows) {
            this.sendError(ws, "bad_cell", `cell outside the pane: ${col},${row}`);
            return;
          }
          await data.stream.mouseClick(col, row);
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
            // `lines <= 0` means "re-send the visible screen, no history": this
            // is how a client returns to the bottom, and it must NOT be read as
            // "use the default", which would page 5000 lines to show one
            // screen.
            const wantsHistory = Number.isFinite(lines) && lines > 0;
            const snap = wantsHistory
              ? await data.stream.snapshotWithHistory(lines)
              : await data.stream.snapshot();
            // Re-check after the await: a newer attach (or a disconnect) may have
            // superseded this request, and repainting another session's screen
            // into the new one is worse than sending nothing.
            if (data.disposed || data.attachGen !== gen) return;
            // The client repaints from scratch, so this is a Snapshot carrying
            // the `history` flag rather than a separate message shape.
            this.flushOutput(ws, data);
            this.sendState(ws, String(data.sessionId ?? ""), snap, { history: wantsHistory });
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
          if (data.live) this.queueOutput(ws, data, text);
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
    this.flushOutput(ws, data);
    this.sendState(ws, sessionId, snap);

    data.live = true;
    const pending = data.pending;
    data.pending = [];
    data.pendingBytes = 0;
    // Whatever queued up while we were snapshotting goes out as one frame.
    for (const text of pending) this.queueOutput(ws, data, text);
    this.flushOutput(ws, data);
  }

  /**
   * Queue pane output, and flush it when the window closes or the buffer grows.
   *
   * Every message this replaces would otherwise have been its own WebSocket
   * frame: one packet, one wakeup and one render on the phone, for output that is
   * usually a single visible line. Merging a burst is strictly cheaper on both
   * ends and changes nothing a user can perceive (8 ms).
   */
  private queueOutput(ws: any, data: ConnData, text: string): void {
    data.outBuf.push(text);
    data.outBytes += text.length;
    if (data.outBytes >= OUTPUT_FLUSH_BYTES) {
      this.flushOutput(ws, data);
      return;
    }
    if (data.flushTimer) return;
    data.flushTimer = setTimeout(() => {
      data.flushTimer = null;
      this.flushOutput(ws, data);
    }, OUTPUT_FLUSH_MS);
  }

  /**
   * Send everything queued as one frame. Called before anything that must land
   * *after* the output (a snapshot in particular): letting a repaint overtake
   * buffered output would show a screen that is already stale.
   */
  private flushOutput(ws: any, data: ConnData): void {
    if (data.flushTimer) {
      clearTimeout(data.flushTimer);
      data.flushTimer = null;
    }
    if (data.outBuf.length === 0) return;
    const text = data.outBuf.length === 1 ? data.outBuf[0] : data.outBuf.join("");
    data.outBuf = [];
    data.outBytes = 0;
    this.sendOutput(ws, text);
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
      mouseSgr: snap.state.mouseSgr,
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
    const buffered = typeof ws.bufferedAmount === "number" ? ws.bufferedAmount : 0;
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

/**
 * Normalise a `ws` message into the string-or-bytes the handler expects.
 *
 * `ws` hands back a Buffer for both kinds, so the only reliable discriminator is
 * its `isBinary` flag (a Buffer that happens to be valid UTF-8 would otherwise be
 * read as JSON text).
 */
function normaliseIncoming(raw: Buffer | ArrayBuffer | Buffer[], isBinary: boolean): string | Uint8Array {
  const bytes = Array.isArray(raw)
    ? Buffer.concat(raw)
    : raw instanceof ArrayBuffer
      ? new Uint8Array(raw)
      : raw;
  return isBinary ? new Uint8Array(bytes) : bytes.toString("utf8");
}

function contentType(path: string): string {
  return CONTENT_TYPES[extname(path).toLowerCase()] ?? "application/octet-stream";
}