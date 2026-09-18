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
import type { DeviceInfo, SessionOwner, SessionRecord } from "../../protocol/src/rpc.ts";
import { tokenMatches } from "./auth.ts";
import { isShellForeground } from "./desktop.ts";
import type { PaneStream, StreamSnapshot } from "./stream.ts";
import { createServer, type IncomingMessage, type Server, type ServerResponse } from "node:http";
import { WebSocketServer, type WebSocket } from "ws";
import type { DesktopInventory } from "../../protocol/src/rpc.ts";


export interface RemoteServerOptions {
  hostname: string;
  /** 0 = ephemeral (tests). */
  port: number;
  token: string;
  getSession(id: string): SessionRecord | undefined;
  listSessions(): SessionInfo[] | Promise<SessionInfo[]>;
  /**
   * Open a new session on the host (FR-10), and answer with what was opened.
   *
   * The record, not `void`: the transport has to tell the caller which session it
   * got (see [CreatedMessage]), and a list cannot say.
   */
  createSession?(
    kind: "shell" | "pi",
    cwd: string | undefined,
    title: string | undefined,
    createdBy: string,
  ): Promise<SessionRecord>;
  /** End a session by id. */
  killSession?(id: string): Promise<void>;
  /**
   * The user closed a shell in the UI, which is not the same as ending it.
   *
   * The daemon decides what that means, because the answer depends on facts only
   * it holds: a shell with a window open on the desktop is kept, and only the
   * caller's claim is released (ADR-0010). Returns whether it actually ended.
   */
  closeSession?(id: string): Promise<{ ok: boolean; ended: boolean; detail?: string }>;
  /**
   * Claim a session for a device (ADR-0010).
   *
   * Returns the holder when someone else has it — the caller answers with who,
   * and the user decides. `takeover` is only ever set after the user was told.
   */
  claim?(
    sessionId: string,
    device: DeviceInfo,
    takeover: boolean,
  ): { ok: true; displaced?: DeviceInfo } | { ok: false; heldBy: SessionOwner };
  /** Drop every claim a device holds — what a closing socket calls. */
  releaseDevice?(deviceId: string): string[];
  /** Let go of one session without ending it. */
  releaseClaim?(sessionId: string, deviceId: string): boolean;
  /** What is on the machine's desktop, for the launcher. */
  desktopInventory?(): Promise<DesktopInventory>;
  /** Put a session's terminal on the desktop's screen, on a chosen workspace. */
  makeLive?(id: string, workspace: string): Promise<{ ok: boolean; detail?: string }>;
  /** Take a session's desktop window away. */
  closeLive?(id: string): Promise<{ ok: boolean; detail?: string }>;
  /** Enter or leave tmux copy-mode for a session. */
  copyMode?(id: string, on: boolean): Promise<void>;
  /** A tmux prefix action (split, new window, rename). */
  acquireStream(record: SessionRecord): PaneStream;
  /** Called when a stream is no longer used (failed attach / closed socket). */
  releaseStream?(record: SessionRecord): void;
  /** Directory of the static web client to serve at /. */
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
  /**
   * Who this connection is, from its `hello` (ADR-0010).
   *
   * Absent until the client identifies itself, and a connection that never does
   * can still browse and create — it simply cannot hold a session, because
   * holding one is a claim and a claim needs a holder to attribute it to.
   */
  device?: DeviceInfo;
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
        // ...and its claims go with it. This is the entire defence against the
        // failure dooboxes calls *Stale State*: a claim that outlives its
        // connection does not merely look wrong, it **blocks the next attach**,
        // so a phone that lost signal would lock its own shell until the daemon
        // restarted. Released here, the shell is claimable the moment anyone
        // asks again.
        if (conn.device) {
          this.byDevice.get(conn.device.id)?.delete(socket);
          // Only when this was the device's LAST connection. A phone holds one
          // socket for the session list and one per open shell, and a reconnect
          // overlaps the old socket with the new one — so releasing on *any*
          // close would let the socket that is already dead free the claim its
          // own replacement has just made. The shell would go quietly claimable
          // by anyone, with no takeover and no message, which is the same class
          // of failure as the *Stale State* this release exists to prevent,
          // pointing the other way.
          if (this.byDevice.get(conn.device.id)?.size === 0) {
            this.byDevice.delete(conn.device.id);
            const freed = this.opts.releaseDevice?.(conn.device.id) ?? [];
            if (freed.length > 0) void this.broadcastSessions().catch(() => {});
          }
        }
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
    // Anything else is not a route. This server was also a static file server for
    // a browser client that no longer exists, which is a second thing to keep
    // correct — path confinement, symlink escapes, content types — for an
    // endpoint whose only real traffic is a WebSocket upgrade. The one sentence
    // stays, so a person who pastes the URL into a browser is told why they see
    // nothing rather than getting an empty page.
    res.writeHead(404, { "content-type": "text/plain; charset=utf-8" });
    res.end("ldmux remote: the endpoint is /ws\n");
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
        case "hello": {
          // Identity, not authentication (ADR-0010): it only has to be stable
          // enough to tell two devices apart in a message about a shell.
          const device = msg.device;
          if (device && typeof device.id === "string" && device.id !== "") {
            data.device = {
              id: device.id.slice(0, 64),
              name: String(device.name ?? "device").slice(0, 32),
              kind: device.kind === "desktop" ? "desktop" : "phone",
            };
            let set = this.byDevice.get(data.device.id);
            if (!set) {
              set = new Set();
              this.byDevice.set(data.device.id, set);
            }
            set.add(ws);
          }
          return;
        }
        case "list":
          await this.sendSessions(ws);
          return;

        case "desktop": {
          // The launcher asks for this. A daemon that cannot answer still
          // answers: an empty inventory with a reason beats an error, because
          // the client can always render a screen from it.
          const inventory = this.opts.desktopInventory
            ? await this.opts.desktopInventory()
            : { wm: "unknown", workspaces: [], windows: [], piSessions: [], detail: "not available" };
          this.sendJson(ws, { type: "desktop", inventory });
          return;
        }
        case "new": {
          // `phone` is a fact about origin, not a label: the client shows a
          // per-shell menu only for shells it opened itself.
          const record = await this.opts.createSession?.(
            msg.kind === "pi" ? "pi" : "shell",
            msg.cwd,
            msg.title,
            "phone",
          );
          if (data.disposed) return;
          // Before the list, so a client that opens what it asked for has the id
          // by the time the list it may also page through arrives.
          if (record) {
            this.sendJson(ws, { type: "created", sessionId: record.id, cwd: record.cwd });
          }
          await this.sendSessions(ws);
          return;
        }
        case "makelive": {
          const id = String(msg.sessionId ?? "");
          const workspace = String(msg.workspace ?? "");
          const result = this.opts.makeLive
            ? await this.opts.makeLive(id, workspace)
            : { ok: false, detail: "not supported by this daemon" };
          this.sendJson(ws, { type: "live", sessionId: id, ...result });
          return;
        }
        case "closelive": {
          const id = String(msg.sessionId ?? "");
          const result = this.opts.closeLive
            ? await this.opts.closeLive(id)
            : { ok: false, detail: "not supported by this daemon" };
          this.sendJson(ws, { type: "live", sessionId: id, ...result });
          return;
        }
        case "kill": {
          await this.opts.killSession?.(String(msg.sessionId ?? ""));
          if (data.disposed) return;
          await this.sendSessions(ws);
          return;
        }
        case "close": {
          // The user closed a shell here. Whether that *ends* it is the daemon's
          // question, because only it knows whether a desktop window is holding
          // it open (ADR-0010). Reusing `kill` for this would make the phone the
          // authority on a fact it cannot see, which is the bug being fixed.
          const id = String(msg.sessionId ?? "");

          // A shell opened at the desk belongs to the desk. The phone can look at
          // it and type in it, but closing it here is putting it down, not
          // destroying it — only where a shell came from may end it. This lives
          // here and not in the daemon's `close`, because the *caller* is what
          // decides, and only the transport knows the caller: the same verb over
          // the local socket is the desk's own CLI, where closing a shell it
          // started must still mean ending it.
          const record = this.opts.getSession(id);
          if (data.device?.kind === "phone" && record && record.createdBy !== "phone") {
            if (data.device) this.opts.releaseClaim?.(id, data.device.id);
            if (data.disposed) return;
            await this.sendSessions(ws);
            return;
          }

          const result = this.opts.closeSession
            ? await this.opts.closeSession(id)
            : { ok: true, ended: true };
          // Let go even when the session survives: a shell held open by the
          // desktop is not one this connection should keep streaming.
          if (data.device && !result.ended) this.opts.releaseClaim?.(id, data.device.id);
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
          // Told, not assumed: the pane's program asked for mouse input, so the
          // click is the whole of what the tap meant. The client would otherwise
          // fall back to cursor motion after its click had already worked.
          this.sendJson(ws, { type: "mousedelivered", col, row });
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
        case "copymode":
          if (data.stream) await data.stream.copyMode(msg.on === true);
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

  /**
   * Sockets by the device they announced themselves as.
   *
   * The connection table is a WeakMap (nothing should keep a socket alive), so
   * it cannot be iterated — but a takeover has to be able to *reach* the device
   * being displaced, which is usually not the connection that caused it. One
   * device may hold several connections (a reconnect overlapping a stale one), so
   * this is a set, and it is cleaned up on close.
   */
  private readonly byDevice = new Map<string, Set<WebSocket>>();

  /**
   * Tell a displaced device it no longer holds a shell, and stop streaming it.
   *
   * Sent to whoever holds that identity, which is usually *not* the connection
   * that caused the takeover — the other device is the one that needs to know.
   * The stream is torn down here rather than left running, because a client that
   * has lost a shell must not keep receiving its output: it would still be able
   * to read it, and it would still be *typing* into it.
   */
  private notifyDisplaced(deviceId: string, sessionId: string, newOwner: DeviceInfo): void {
    const holders = this.byDevice.get(deviceId);
    // Logged because "who took my shell" is the first question a user asks, and
    // the answer has to outlive the moment it happened.
    this.opts.log?.(
      `remote: ${newOwner.name} took ${sessionId} from ${deviceId} ` +
        `(${holders?.size ?? 0} connection(s) held by that device)`,
    );
    for (const client of holders ?? []) {
      const conn = this.connState.get(client);
      if (!conn || conn.sessionId !== sessionId) continue;
      this.sendJson(client, {
        type: "owned",
        sessionId,
        // Who has it *now*, so the displaced device can say so by name rather
        // than with a shrug.
        owner: { ...newOwner, since: Date.now() },
      });
      this.cleanupSubscription(conn);
      if (conn.record) this.opts.releaseStream?.(conn.record);
      conn.sessionId = undefined;
      conn.record = undefined;
      conn.live = false;
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

    // Exclusivity (ADR-0010). Order matters: the refusal has to happen before
    // anything is torn down, or a plain attach to somebody else's shell would
    // displace the *caller's* own current attach while failing.
    if (data.device && this.opts.claim) {
      const claimed = this.opts.claim(sessionId, data.device, msg.takeover === true);
      if (!claimed.ok) {
        // Not an error: it is the ordinary answer to "open this", and the client
        // answers it by telling the user who has it and offering to take over.
        this.sendJson(ws, { type: "owned", sessionId, owner: claimed.heldBy });
        return;
      }
      if (claimed.displaced) this.notifyDisplaced(claimed.displaced.id, sessionId, data.device);
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

  /**
   * Push the session list to every connected client.
   *
   * The pager must not go stale. Sessions appear and disappear on their own — a
   * shell exits, someone opens one elsewhere — and a client that only learns the
   * list from replies to its own requests keeps showing pages that no longer
   * exist, or misses ones that do. This is "they pop up and sink" made visible:
   * the fix is to tell every client when the list actually changes.
   */
  async broadcastSessions(): Promise<void> {
    if (!this.wss || this.wss.clients.size === 0) return;
    const msg = JSON.stringify({ type: "sessions", sessions: await this.opts.listSessions() });
    for (const client of this.wss.clients) {
      // 1 === OPEN: a socket mid-handshake or closing is not worth waiting for.
      if (client.readyState === 1) client.send(msg);
    }
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
