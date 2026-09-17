/**
 * ldmux daemon: owns one private tmux server, keeps a session registry
 * reconciled against it, and serves a newline-delimited JSON API on a Unix
 * socket. The CLI and the mobile/web clients are clients of this same API.
 *
 * Single-instance guarantee: a pid lockfile (`ldmuxd.lock`) serialises startup
 * so concurrent spawn attempts cannot race two daemons into existence or clobber
 * each other's socket.
 */

import { chmodSync, closeSync, existsSync, mkdirSync, openSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { createServer, type Server, type Socket } from "node:net";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";
import { isValidSessionName, TmuxControlPlane } from "./tmux/control-plane.ts";
import { Registry } from "./registry.ts";
import { PaneStream } from "./stream.ts";
import { RemoteServer } from "./remote.ts";
import { readDesktop } from "./desktop-inventory.ts";
import { closeLive, openLive } from "./desktop.ts";
import type { DesktopInventory } from "../../protocol/src/rpc.ts";
import { loadOrCreateToken } from "./auth.ts";
import type {
  NewParams,
  SendParams,
  SessionKind,
  SessionRecord,
} from "../../protocol/src/rpc.ts";
import { DAEMON_PROTOCOL_VERSION } from "../../protocol/src/rpc.ts";
import type { SessionInfo } from "../../protocol/src/messages.ts";

export interface LdmuxDaemonOptions {
  /** Directory for ldmuxd.sock + registry.json + ldmuxd.lock. */
  stateDir: string;
  /** tmux -L socket name. */
  tmuxSocket?: string;
  canonical?: { cols: number; rows: number };
  historyLimit?: number;
  /** Dead records older than this are pruned (default 24h). */
  deadRecordTtlMs?: number;
  /** Hard cap on registry size before oldest records are pruned (default 500). */
  maxRecords?: number;
  /** Remote WebSocket bind host (default 127.0.0.1 — never a public interface). */
  remoteHost?: string;
  /** Remote WebSocket port; 0 picks an ephemeral port (tests). */
  remotePort?: number;
  /** Set false to run without the remote transport (tests). */
  enableRemote?: boolean;
  /** Static web client root to serve at / (defaults to the bundled client). */
  webRoot?: string;
}

interface RpcRequest {
  id?: number;
  method: string;
  params?: Record<string, unknown>;
}

interface Conn {
  buf: string;
  /**
   * Incremental decoder: one per connection, decoded with `{stream: true}`, so a
   * multi-byte UTF-8 character split across two reads is reassembled rather than
   * silently corrupted into U+FFFD (the result would still parse as JSON).
   */
  decoder: TextDecoder;
  /** Serializes dispatch: handlers are async, so responses must stay ordered. */
  queue: Promise<void>;
  write(data: string): void;
  close(): void;
}

const shortId = () => crypto.randomUUID().replaceAll("-", "").slice(0, 10);

const SESSION_KINDS: readonly SessionKind[] = ["shell", "pi"];

/**
 * How long a desktop reading stays fresh.
 *
 * Long enough that a launcher opening, closing and re-opening does not repeat
 * the probes, short enough that a window closed by hand is gone by the time
 * anyone looks again.
 */
const DESKTOP_TTL_MS = 1000;

/**
 * `kind` is stored on the record and rendered by clients, so it is validated at
 * the boundary rather than trusted from the caller (the CLI forwards argv
 * straight through, and registry.json is loaded without validation).
 */
function parseKind(value: unknown): SessionKind {
  const kind = value ?? "shell";
  if (typeof kind !== "string" || !SESSION_KINDS.includes(kind as SessionKind)) {
    throw new Error(`invalid kind: ${JSON.stringify(value)} (expected shell or pi)`);
  }
  return kind as SessionKind;
}

/** Default titles say nothing; the working directory does. */
function sessionTitle(record: SessionRecord): string {
  if (record.title && record.title !== "shell" && record.title !== "pi") return record.title;
  return basename(record.cwd) || record.title || record.id;
}

/** The bundled web client ships beside the daemon package. */
function defaultWebRoot(): string {
  try {
    return fileURLToPath(new URL("../../web/public", import.meta.url));
  } catch {
    return join(process.cwd(), "packages", "web", "public");
  }
}

export class LdmuxDaemon {
  readonly socketPath: string;
  readonly cp: TmuxControlPlane;
  readonly registry: Registry;
  readonly stateDir: string;
  private readonly lockPath: string;
  private readonly canonical: { cols: number; rows: number };
  private readonly historyLimit: number;
  private readonly deadRecordTtlMs: number;
  private readonly maxRecords: number;
  private ownsLock = false;
  private server: Server | undefined;
  private readonly conns = new Map<unknown, Conn>();
  private remote: RemoteServer | undefined;
  private readonly streams = new Map<string, PaneStream>();
  private remoteInfo: { hostname: string; port: number } | undefined;
  private token = "";

  private readonly opts: LdmuxDaemonOptions;

  constructor(opts: LdmuxDaemonOptions) {
    this.opts = opts;
    this.stateDir = opts.stateDir;
    mkdirSync(this.stateDir, { recursive: true, mode: 0o700 });
    this.socketPath = join(this.stateDir, "ldmuxd.sock");
    this.lockPath = join(this.stateDir, "ldmuxd.lock");
    this.cp = new TmuxControlPlane(opts.tmuxSocket ?? "ldmux");
    this.registry = new Registry(join(this.stateDir, "registry.json"));
    this.canonical = opts.canonical ?? { cols: 120, rows: 40 };
    // 50k lines costs ~200 MB/pane (dooboxes' measurement); 10k is ~40 MB.
    this.historyLimit = opts.historyLimit ?? 10000;
    this.deadRecordTtlMs = opts.deadRecordTtlMs ?? 24 * 60 * 60 * 1000;
    this.maxRecords = opts.maxRecords ?? 500;
  }

  /**
   * Start the daemon. Throws "ldmuxd already running" if another live daemon
   * holds the lock (callers should treat that as success — someone else won).
   */
  async start(): Promise<void> {
    if (!this.acquireLock()) throw new Error("ldmuxd already running");
    this.ownsLock = true;
    try {
      await this.cp.ensureServer({ historyLimit: this.historyLimit });
      this.registry.load();
      await this.reconcile();
      try {
        rmSync(this.socketPath);
      } catch {
        /* no stale socket */
      }
      this.server = createServer((socket) => {
        const conn = this.makeConn(socket);
        this.conns.set(socket, conn);
        socket.on("data", (chunk: Buffer) => this.onData(socket, chunk));
        const forget = () => this.conns.delete(socket);
        socket.on("close", forget);
        socket.on("error", forget);
      });
      // The socket is a full-control channel: owner-only. The umask closes the
      // window between the bind (which creates the file with the process umask,
      // typically 0755) and the chmod below — otherwise another local user could
      // connect to a control channel in that instant.
      const previousUmask = process.umask(0o077);
      try {
        await new Promise<void>((resolve, reject) => {
          const onError = (err: Error) => reject(err);
          this.server!.once("error", onError);
          this.server!.listen(this.socketPath, () => {
            this.server!.off("error", onError);
            resolve();
          });
        });
      } finally {
        process.umask(previousUmask);
      }
      chmodSync(this.socketPath, 0o600);
      // Keep an error listener for the server's whole life: an 'error' event
      // with no listener is rethrown by EventEmitter, which would take the
      // daemon down (the remote HTTP server does the same).
      this.server.on("error", (err: Error) => {
        console.error(`ldmuxd: control socket error: ${err.message}`);
      });
      await this.startRemote();
    } catch (err) {
      this.releaseLock();
      throw err;
    }
  }

  private async startRemote(): Promise<void> {
    if (this.opts.enableRemote === false) return;
    try {
      this.token = loadOrCreateToken(this.stateDir);
      this.remote = new RemoteServer({
        hostname: this.opts.remoteHost ?? "127.0.0.1",
        port: this.opts.remotePort ?? 7623,
        token: this.token,
        getSession: (id) => this.registry.get(id),
        listSessions: () => this.describeSessions(),
        createSession: async (kind, cwd, title, createdBy) => {
          await this.handleNew({
            kind,
            cwd: cwd ?? process.cwd(),
            title,
            createdBy,
          } as unknown as NewParams);
        },
        makeLive: (id, workspace) => this.handle("makeLive", { id, workspace }) as Promise<{ ok: boolean; detail?: string }>,
        closeLive: (id) => this.handle("closeLive", { id }) as Promise<{ ok: boolean; detail?: string }>,
        copyMode: async (id: string, on: boolean) => {
          await this.handle("copyMode", { id, on });
        },
        tmuxAction: async (id: string, action: string, name?: string) => {
          await this.handle("tmuxAction", { id, action, name });
        },
        killSession: async (id) => {
          await this.handle("kill", { id });
        },
        desktopInventory: () => this.desktopInventory(),
        acquireStream: (record) => this.streamFor(record),
        releaseStream: (record) => this.releaseStreamIfIdle(record.tmuxName),
        webRoot: this.opts.webRoot ?? defaultWebRoot(),
        log: (msg) => console.error(msg),
      });
      this.remoteInfo = await this.remote.start();
    } catch (err) {
      // Remote is optional: a bind failure must not take down local control.
      console.error(`ldmuxd: remote disabled: ${(err as Error).message}`);
      this.remote = undefined;
      this.remoteInfo = undefined;
    }
  }

  /**
   * The session list as remote clients see it: registry metadata enriched with
   * tmux's own activity clock (so a client can show an unread marker) and with
   * a title derived from the working directory when the record has only the
   * default one.
   */
  private async describeSessions(): Promise<SessionInfo[]> {
    // A read-only listing must not mutate stream state.
    await this.reconcile({ sweep: false });
    const live = await this.cp.listSessions();
    const activity = new Map<string, number>();
    if (live) for (const s of live) activity.set(s.name, s.activity * 1000);
    return this.registry.list().map((record) => ({
      id: record.id,
      kind: record.kind,
      title: sessionTitle(record),
      cwd: record.cwd,
      status: record.status,
      tmuxName: record.tmuxName,
      // Who opened it. The phone shows a per-shell menu only for shells it
      // opened itself, so this has to survive to the client.
      createdBy: record.createdBy,
      lastActivityAt:
        (record.tmuxName ? activity.get(record.tmuxName) : undefined) ?? record.updatedAt,
    }));
  }

  /** The bound remote endpoint, when remote started successfully. */
  remoteInfoForTest(): { hostname: string; port: number } {
    return this.remoteInfo ?? { hostname: "127.0.0.1", port: 0 };
  }

  /** Get-or-create the live stream for a terminal-backed session. */
  private streamFor(record: SessionRecord): PaneStream {
    let stream = this.streams.get(record.tmuxName);
    if (!stream) {
      stream = new PaneStream(this.cp.socket, record.tmuxName, this.cp);
      this.streams.set(record.tmuxName, stream);
    }
    return stream;
  }

  /** Close and forget a stream (its tmux session is gone). */
  private dropStream(tmuxName: string, reason = "session ended"): void {
    const stream = this.streams.get(tmuxName);
    if (!stream) return;
    this.streams.delete(tmuxName);
    stream.close(reason);
  }

  /** Release a stream only if nothing is subscribed to it. */
  private releaseStreamIfIdle(tmuxName: string): void {
    const stream = this.streams.get(tmuxName);
    if (stream && stream.subscriberCount === 0) {
      this.streams.delete(tmuxName);
      stream.close();
    }
  }

  /** Evict streams whose backing session is gone (bounded memory). */
  private sweepStreams(): void {
    for (const [tmuxName, stream] of this.streams) {
      const record = this.registry.byTmuxName(tmuxName);
      if (!record || record.status === "dead") this.dropStream(tmuxName);
    }
  }

  stop(): void {
    try {
      this.remote?.stop();
    } catch {
      /* ignore */
    }
    for (const stream of this.streams.values()) stream.close();
    this.streams.clear();
    try {
      this.server?.close();
    } catch {
      /* ignore */
    }
    for (const conn of this.conns.values()) {
      try {
        conn.close();
      } catch {
        /* ignore */
      }
    }
    this.conns.clear();
    try {
      rmSync(this.socketPath);
    } catch {
      /* ignore */
    }
    this.releaseLock();
  }

  // ── connection handling ────────────────────────────────────────────────

  private makeConn(socket: Socket): Conn {
    return {
      buf: "",
      decoder: new TextDecoder(),
      queue: Promise.resolve(),
      write: (data: string) => this.safeWrite(socket, data),
      close: () => socket.destroy(),
    };
  }

  /** Never let a client disconnect mid-write crash the daemon. */
  private safeWrite(socket: Socket, data: string): void {
    try {
      socket.write(data);
    } catch {
      /* client went away */
    }
  }

  private onData(socket: Socket, data: Uint8Array): void {
    const conn = this.conns.get(socket);
    if (!conn) return;
    conn.buf += conn.decoder.decode(data, { stream: true });
    let nl: number;
    while ((nl = conn.buf.indexOf("\n")) >= 0) {
      const line = conn.buf.slice(0, nl);
      conn.buf = conn.buf.slice(nl + 1);
      if (line.trim()) {
        // Chain, never start two handlers on one connection at once: handle()
        // awaits tmux now, and interleaving would reorder responses.
        conn.queue = conn.queue
          .then(() => this.dispatch(socket, conn, line))
          .catch(() => {
            /* dispatch never throws, but never surface an unhandled rejection */
          });
      }
    }
  }

  private async dispatch(ws: any, conn: Conn, line: string): Promise<void> {
    let req: RpcRequest;
    try {
      req = JSON.parse(line) as RpcRequest;
    } catch {
      this.safeWrite(ws, JSON.stringify({ id: null, ok: false, error: "invalid JSON" }) + "\n");
      return;
    }
    let response: unknown;
    try {
      response = { id: req.id ?? null, ok: true, result: await this.handle(req.method, req.params ?? {}) };
    } catch (err) {
      response = { id: req.id ?? null, ok: false, error: (err as Error).message };
    }
    this.safeWrite(ws, JSON.stringify(response) + "\n");
  }

  // ── lock ───────────────────────────────────────────────────────────────

  private acquireLock(): boolean {
    const create = () => {
      const fd = openSync(this.lockPath, "wx");
      try {
        writeFileSync(fd, String(process.pid));
      } finally {
        closeSync(fd);
      }
    };
    try {
      create();
      return true;
    } catch (err) {
      if ((err as NodeJS.ErrnoException).code !== "EEXIST") throw err;
    }
    // Lock exists: steal it only if the recorded pid is dead.
    let pid = 0;
    try {
      pid = Number(readFileSync(this.lockPath, "utf8").trim());
    } catch {
      /* unreadable */
    }
    let alive = false;
    if (Number.isInteger(pid) && pid > 0) {
      try {
        process.kill(pid, 0);
        alive = true;
      } catch {
        alive = false;
      }
    }
    if (alive) return false;
    try {
      rmSync(this.lockPath);
    } catch {
      /* ignore */
    }
    try {
      create();
      return true;
    } catch {
      return false;
    }
  }

  private releaseLock(): void {
    if (!this.ownsLock) return;
    this.ownsLock = false;
    try {
      rmSync(this.lockPath);
    } catch {
      /* ignore */
    }
  }

  // ── registry reconciliation ──────────────────────────────────────────────

  private async reconcile(
    opts: { sweep?: boolean } = {},
  ): Promise<{ dead: string[]; adopted: string[]; pruned: string[] }> {
    const liveSessions = await this.cp.listSessions();
    const live = liveSessions ? new Set(liveSessions.map((s) => s.name)) : null;
    const { dead, adopted } = this.registry.reconcile(live);
    const pruned = this.registry.prune({ maxDeadAgeMs: this.deadRecordTtlMs, maxRecords: this.maxRecords });
    if (dead.length || adopted.length || pruned.length) {
      this.registry.save();
      // The set of live sessions changed, so every connected client's pager is
      // now wrong. Tell them, rather than waiting for a request that may never
      // come: a page whose session died is a page that silently does nothing.
      void this.remote?.broadcastSessions().catch(() => {});
    }
    // Only sweep on a *known* live set, and never from a read-only listing.
    if (opts.sweep !== false && live) this.sweepStreams();
    return { dead, adopted, pruned };
  }

  /**
   * The desktop inventory, briefly cached.
   *
   * Building it spawns `i3-msg`/`xprop` and walks `/proc`, which is cheap but
   * not free; a launcher opening, closing and re-opening within a moment should
   * not repeat all of it. A second is short enough that a window closed by hand
   * is gone by the time anyone looks again.
   */
  private desktopCache: { at: number; value: Promise<DesktopInventory> } | undefined;

  private desktopInventory(): Promise<DesktopInventory> {
    const now = Date.now();
    if (this.desktopCache && now - this.desktopCache.at < DESKTOP_TTL_MS) {
      return this.desktopCache.value;
    }
    // The promise is cached, not its value: concurrent callers share one read
    // instead of stacking up probes.
    const value = readDesktop().catch((err: unknown) => {
      this.desktopCache = undefined;
      throw err;
    });
    this.desktopCache = { at: now, value };
    return value;
  }

  private require(id: string): SessionRecord {
    const record = this.registry.get(id);
    if (!record) throw new Error(`unknown session: ${id}`);
    return record;
  }

  /**
   * Resolve a session that has a terminal. An RPC-only pi session has no tmux
   * session, and running tmux with an empty target produces a transport error
   * that says nothing about the real problem.
   */
  private requireTerminal(id: string): SessionRecord {
    const record = this.require(id);
    if (!record.tmuxName) {
      throw new Error(`session ${id} has no terminal attached`);
    }
    return record;
  }

  // ── RPC methods ──────────────────────────────────────────────────────────

  private async handle(method: string, params: Record<string, unknown>): Promise<unknown> {
    switch (method) {
      case "ping":
        return { pong: true, version: DAEMON_PROTOCOL_VERSION, tmuxSocket: this.cp.socket };

      case "desktop":
        return this.desktopInventory();

      case "makeLive": {
        const p = params as { id?: unknown; workspace?: unknown };
        const record = this.requireTerminal(String(p.id ?? ""));
        // A dead session still has a tmux name, so `requireTerminal` is happy —
        // but attaching to it fails and the window closes the instant it opens,
        // which looks exactly like the button doing nothing. Refuse it here,
        // where there is something true to say.
        if (record.status !== "running") {
          return { ok: false, detail: `that shell is ${record.status}` };
        }
        const workspace = String(p.workspace ?? "");
        // Best-effort by design: not every host has a window manager, and a phone
        // must not be told its shell is broken because the desktop is headless.
        return await openLive(this.cp.socket, record.tmuxName, record.id, workspace, this.cp.bin);
      }

      case "closeLive": {
        const record = this.require(String((params as { id?: unknown }).id ?? ""));
        return await closeLive(record.id);
      }

      case "remote_info":
        return {
          hostname: this.remoteInfo?.hostname ?? "127.0.0.1",
          port: this.remoteInfo?.port ?? 0,
          path: "/ws",
          token: this.token,
        };

      case "list": {
        await this.reconcile();
        return { sessions: this.registry.list() };
      }

      case "new":
        return this.handleNew(params as unknown as NewParams);

      case "send": {
        const p = params as unknown as SendParams;
        const record = this.requireTerminal(p.id);
        if (typeof p.text === "string") await this.cp.sendText(record.tmuxName, p.text);
        if (p.keys?.length) await this.cp.sendKey(record.tmuxName, ...p.keys);
        return { ok: true };
      }

      case "capture":
        return {
          screen: await this.cp.capturePane(this.requireTerminal(String(params.id)).tmuxName, {
            history: Boolean(params.history),
          }),
        };

      case "copyMode": {
        // Straight at the pane, not through the stream: copy-mode is a tmux
        // state, and a session nobody has open should still be able to enter it.
        const record = this.requireTerminal(String(params.id));
        await this.cp.copyMode(record.tmuxName, Boolean(params.on));
        return { ok: true };
      }

      case "tmuxAction": {
        const record = this.requireTerminal(String(params.id));
        const action = String(params.action ?? "");
        if (
          action !== "split-right" &&
          action !== "split-down" &&
          action !== "new-window" &&
          action !== "rename"
        ) {
          throw new Error(`unknown tmux action: ${action}`);
        }
        await this.cp.tmuxAction(
          record.tmuxName,
          action,
          typeof params.name === "string" ? params.name : undefined,
        );
        return { ok: true };
      }

      case "snapshot": {
        const record = this.requireTerminal(String(params.id));
        const [screen, state] = await Promise.all([
          this.cp.capturePane(record.tmuxName),
          this.cp.snapshotState(record.tmuxName),
        ]);
        return { screen, state };
      }

      case "kill": {
        const record = this.require(String(params.id));
        // An RPC-only session has nothing in tmux to kill; ending it still has
        // to mark the record dead so the list does not keep showing it.
        if (record.tmuxName) {
          await this.cp.killSession(record.tmuxName);
          this.dropStream(record.tmuxName);
        }
        record.status = "dead";
        record.exitedAt = Date.now();
        record.updatedAt = Date.now();
        this.registry.save();
        return { ok: true, session: record };
      }

      case "forget": {
        // Deliberately does not touch tmux: `kill` ends a session, `forget`
        // only removes the registry entry (a dead record or an adopted one the
        // user no longer wants to see).
        const id = String(params.id);
        const record = this.registry.get(id);
        if (!record) return { ok: true, removed: false };
        if (record.tmuxName) this.dropStream(record.tmuxName, "session forgotten");
        this.registry.remove(id);
        this.registry.save();
        return { ok: true, removed: true };
      }

      case "adopt":
        return { session: await this.adopt(params) };

      default:
        throw new Error(`unknown method: ${method}`);
    }
  }

  private async handleNew(p: NewParams): Promise<{ session: SessionRecord }> {
    const kind = parseKind(p.kind);
    const cwd = String(p.cwd ?? process.cwd());
    if (!existsSync(cwd) || !statSync(cwd).isDirectory()) throw new Error(`not a directory: ${cwd}`);
    const id = shortId();
    const tmuxName = `ldmux-${id}`;
    const command = kind === "pi" ? ["pi", ...(p.command ?? [])] : (p.command ?? []);
    const now = Date.now();
    const record: SessionRecord = {
      id,
      tmuxName,
      kind,
      cwd,
      title: String(p.title ?? (kind === "pi" ? "pi" : "shell")),
      createdAt: now,
      updatedAt: now,
      createdBy: String(p.createdBy ?? "local"),
      status: "running",
    };
    // Child env contract (docs/SPEC.md §7) injected into the session command.
    const env = {
      LDMUX: "1",
      LDMUX_SESSION_ID: id,
      LDMUX_KIND: kind,
      LDMUX_SOCKET: this.socketPath,
      LDMUX_TMUX_NAME: tmuxName,
      LDMUX_TMUX_SOCK: this.cp.socket,
    };
    await this.cp.newSession({
      name: tmuxName,
      cwd,
      command,
      env,
      destroyUnattached: Boolean(p.ephemeral),
      ...this.canonical,
    });
    this.registry.upsert(record);
    this.registry.save();
    return { session: record };
  }

  private async adopt(params: Record<string, unknown>): Promise<SessionRecord> {
    const tmuxName = String(params.tmuxName);
    if (!isValidSessionName(tmuxName)) throw new Error(`invalid tmux session name: ${tmuxName}`);
    if (!(await this.cp.hasSession(tmuxName))) throw new Error(`no such tmux session: ${tmuxName}`);
    const existing = this.registry.byTmuxName(tmuxName);
    if (existing) return existing;
    const panes = await this.cp.listPanes(tmuxName);
    // Re-check after the awaits: handle() is async now, so two concurrent
    // adopts of one tmux session could otherwise each create a record.
    const raced = this.registry.byTmuxName(tmuxName);
    if (raced) return raced;
    const now = Date.now();
    const record: SessionRecord = {
      id: shortId(),
      tmuxName,
      kind: parseKind(params.kind),
      cwd: panes[0]?.cwd ?? process.cwd(),
      title: String(params.title ?? tmuxName),
      createdAt: now,
      updatedAt: now,
      createdBy: "adopt",
      status: "running",
    };
    this.registry.upsert(record);
    this.registry.save();
    return record;
  }
}