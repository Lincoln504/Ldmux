/**
 * ldmux daemon: owns one private tmux server, keeps a session registry
 * reconciled against it, and serves a newline-delimited JSON API on a Unix
 * socket. The pi extension and CLI are clients of this same API.
 *
 * Single-instance guarantee: a pid lockfile (`ldmuxd.lock`) serialises startup
 * so concurrent spawn attempts (e.g. several pi sessions starting at once)
 * cannot race two daemons into existence or clobber each other's socket.
 */

import { chmodSync, closeSync, existsSync, mkdirSync, openSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { TmuxControlPlane } from "./tmux/control-plane.ts";
import { Registry } from "./registry.ts";
import type {
  NewParams,
  RegisterParams,
  SendParams,
  SessionEndParams,
  SessionKind,
  SessionRecord,
} from "../../protocol/src/rpc.ts";
import { DAEMON_PROTOCOL_VERSION } from "../../protocol/src/rpc.ts";

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
}

interface RpcRequest {
  id?: number;
  method: string;
  params?: Record<string, unknown>;
}

interface Conn {
  buf: string;
  write(data: string): void;
  close(): void;
}

const shortId = () => crypto.randomUUID().replaceAll("-", "").slice(0, 10);

export class LdmuxDaemon {
  readonly socketPath: string;
  readonly cp: TmuxControlPlane;
  readonly registry: Registry;
  private readonly stateDir: string;
  private readonly lockPath: string;
  private readonly canonical: { cols: number; rows: number };
  private readonly historyLimit: number;
  private readonly deadRecordTtlMs: number;
  private readonly maxRecords: number;
  private ownsLock = false;
  private server: { stop(closeActive?: boolean): void } | undefined;
  private readonly conns = new Map<unknown, Conn>();

  constructor(private readonly opts: LdmuxDaemonOptions) {
    this.stateDir = opts.stateDir;
    mkdirSync(this.stateDir, { recursive: true, mode: 0o700 });
    this.socketPath = join(this.stateDir, "ldmuxd.sock");
    this.lockPath = join(this.stateDir, "ldmuxd.lock");
    this.cp = new TmuxControlPlane(opts.tmuxSocket ?? "ldmux");
    this.registry = new Registry(join(this.stateDir, "registry.json"));
    this.canonical = opts.canonical ?? { cols: 120, rows: 40 };
    this.historyLimit = opts.historyLimit ?? 50000;
    this.deadRecordTtlMs = opts.deadRecordTtlMs ?? 24 * 60 * 60 * 1000;
    this.maxRecords = opts.maxRecords ?? 500;
  }

  /**
   * Start the daemon. Throws "ldmuxd already running" if another live daemon
   * holds the lock (callers should treat that as success — someone else won).
   */
  start(): void {
    if (!this.acquireLock()) throw new Error("ldmuxd already running");
    this.ownsLock = true;
    try {
      this.cp.ensureServer({ historyLimit: this.historyLimit });
      this.registry.load();
      this.reconcile();
      try {
        rmSync(this.socketPath);
      } catch {
        /* no stale socket */
      }
      this.server = Bun.listen({
        unix: this.socketPath,
        socket: {
          open: (ws: any) => this.conns.set(ws, this.makeConn(ws)),
          data: (ws: any, data: Uint8Array) => this.onData(ws, data),
          close: (ws: any) => this.conns.delete(ws),
          error: (ws: any) => this.conns.delete(ws),
        },
      });
      // The socket is a full-control channel: owner-only.
      chmodSync(this.socketPath, 0o600);
    } catch (err) {
      this.releaseLock();
      throw err;
    }
  }

  stop(): void {
    try {
      this.server?.stop(true);
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

  private makeConn(ws: any): Conn {
    return {
      buf: "",
      write: (data: string) => this.safeWrite(ws, data),
      close: () => ws.close(),
    };
  }

  /** Never let a client disconnect mid-write crash the daemon. */
  private safeWrite(ws: any, data: string): void {
    try {
      ws.write(data);
    } catch {
      /* client went away */
    }
  }

  private onData(ws: any, data: Uint8Array): void {
    const conn = this.conns.get(ws);
    if (!conn) return;
    conn.buf += new TextDecoder().decode(data);
    let nl: number;
    while ((nl = conn.buf.indexOf("\n")) >= 0) {
      const line = conn.buf.slice(0, nl);
      conn.buf = conn.buf.slice(nl + 1);
      if (line.trim()) {
        void this.dispatch(ws, conn, line).catch(() => {
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
      response = { id: req.id ?? null, ok: true, result: this.handle(req.method, req.params ?? {}) };
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

  private reconcile(): { dead: string[]; adopted: string[]; pruned: string[] } {
    const live = new Set(this.cp.listSessions().map((s) => s.name));
    const { dead, adopted } = this.registry.reconcile(live);
    const pruned = this.registry.prune({ maxDeadAgeMs: this.deadRecordTtlMs, maxRecords: this.maxRecords });
    if (dead.length || adopted.length || pruned.length) this.registry.save();
    return { dead, adopted, pruned };
  }

  private require(id: string): SessionRecord {
    const record = this.registry.get(id);
    if (!record) throw new Error(`unknown session: ${id}`);
    return record;
  }

  // ── RPC methods ──────────────────────────────────────────────────────────

  private handle(method: string, params: Record<string, unknown>): unknown {
    switch (method) {
      case "ping":
        return { pong: true, version: DAEMON_PROTOCOL_VERSION, tmuxSocket: this.cp.socket };

      case "list": {
        this.reconcile();
        return { sessions: this.registry.list() };
      }

      case "new":
        return this.handleNew(params as unknown as NewParams);

      case "send": {
        const p = params as unknown as SendParams;
        const record = this.require(p.id);
        if (typeof p.text === "string") this.cp.sendText(record.tmuxName, p.text);
        if (p.keys?.length) this.cp.sendKey(record.tmuxName, ...p.keys);
        return { ok: true };
      }

      case "capture":
        return {
          screen: this.cp.capturePane(this.require(String(params.id)).tmuxName, {
            history: Boolean(params.history),
          }),
        };

      case "snapshot": {
        const record = this.require(String(params.id));
        return { screen: this.cp.capturePane(record.tmuxName), state: this.cp.snapshotState(record.tmuxName) };
      }

      case "kill": {
        const record = this.require(String(params.id));
        this.cp.killSession(record.tmuxName);
        record.status = "dead";
        record.exitedAt = Date.now();
        record.updatedAt = Date.now();
        this.registry.save();
        return { ok: true, session: record };
      }

      case "adopt":
        return { session: this.adopt(params) };

      case "register":
        return { session: this.register(params as unknown as RegisterParams) };

      case "session_end":
        return this.endSession(params as unknown as SessionEndParams);

      default:
        throw new Error(`unknown method: ${method}`);
    }
  }

  private handleNew(p: NewParams): { session: SessionRecord } {
    const kind: SessionKind = p.kind ?? "shell";
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
    this.cp.newSession({ name: tmuxName, cwd, command, env, ...this.canonical });
    this.registry.upsert(record);
    this.registry.save();
    return { session: record };
  }

  private adopt(params: Record<string, unknown>): SessionRecord {
    const tmuxName = String(params.tmuxName);
    if (!this.cp.hasSession(tmuxName)) throw new Error(`no such tmux session: ${tmuxName}`);
    const existing = this.registry.byTmuxName(tmuxName);
    if (existing) return existing;
    const panes = this.cp.listPanes(tmuxName);
    const now = Date.now();
    const record: SessionRecord = {
      id: shortId(),
      tmuxName,
      kind: (params.kind as SessionKind) ?? "shell",
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

  /** Bind a pi session to its (possibly RPC-only) ldmux record. Idempotent. */
  private register(p: RegisterParams): SessionRecord {
    const now = Date.now();
    const tmuxName = p.tmuxName ?? "";
    const existing = this.registry.byPiSessionId(p.piSessionId) ?? this.registry.byTmuxName(tmuxName);
    if (existing) {
      existing.piSessionId = p.piSessionId;
      existing.cwd = p.cwd;
      existing.status = "running";
      existing.exitedAt = null;
      existing.updatedAt = now;
      if (tmuxName) existing.tmuxName = tmuxName;
      this.registry.save();
      return existing;
    }
    const record: SessionRecord = {
      id: shortId(),
      tmuxName,
      kind: p.kind ?? "pi",
      cwd: p.cwd,
      title: String(p.title ?? "pi"),
      createdAt: now,
      updatedAt: now,
      createdBy: "pi-extension",
      status: "running",
      piSessionId: p.piSessionId,
    };
    this.registry.upsert(record);
    this.registry.save();
    return record;
  }

  private endSession(p: SessionEndParams): { ok: true; session?: SessionRecord } {
    const record = this.registry.byPiSessionId(p.piSessionId);
    if (!record) return { ok: true };
    record.status = "dead";
    record.exitedAt = Date.now();
    record.updatedAt = Date.now();
    this.registry.save();
    return { ok: true, session: record };
  }
}