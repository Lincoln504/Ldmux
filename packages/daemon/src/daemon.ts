/**
 * ldmux daemon: owns one private tmux server, keeps a session registry
 * reconciled against it, and serves a newline-delimited JSON API on a Unix
 * socket (CLI/local) and, in a later phase, over the tailnet.
 */

import { existsSync, mkdirSync, rmSync, statSync } from "node:fs";
import { join } from "node:path";
import { TmuxControlPlane } from "./tmux/control-plane.ts";
import { Registry, type SessionKind, type SessionRecord } from "./registry.ts";

export interface LdmuxDaemonOptions {
  /** Directory for ldmuxd.sock + registry.json. */
  stateDir: string;
  /** tmux -L socket name. */
  tmuxSocket?: string;
  canonical?: { cols: number; rows: number };
  historyLimit?: number;
}

interface RpcRequest {
  id?: number;
  method: string;
  params?: Record<string, unknown>;
}

const shortId = () => crypto.randomUUID().replaceAll("-", "").slice(0, 10);

export class LdmuxDaemon {
  readonly socketPath: string;
  readonly cp: TmuxControlPlane;
  readonly registry: Registry;
  private readonly canonical: { cols: number; rows: number };
  private readonly historyLimit: number;
  private server: any;
  private readonly conns = new Map<any, { buf: string }>();

  constructor(private readonly opts: LdmuxDaemonOptions) {
    mkdirSync(opts.stateDir, { recursive: true });
    this.socketPath = join(opts.stateDir, "ldmuxd.sock");
    this.cp = new TmuxControlPlane(opts.tmuxSocket ?? "ldmux");
    this.registry = new Registry(join(opts.stateDir, "registry.json"));
    this.canonical = opts.canonical ?? { cols: 120, rows: 40 };
    this.historyLimit = opts.historyLimit ?? 50000;
  }

  start(): void {
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
        open: (ws: any) => this.conns.set(ws, { buf: "" }),
        data: (ws: any, data: Uint8Array) => {
          const conn = this.conns.get(ws);
          if (!conn) return;
          conn.buf += new TextDecoder().decode(data);
          let nl: number;
          while ((nl = conn.buf.indexOf("\n")) >= 0) {
            const line = conn.buf.slice(0, nl);
            conn.buf = conn.buf.slice(nl + 1);
            if (line.trim()) void this.dispatch(ws, line);
          }
        },
        close: (ws: any) => this.conns.delete(ws),
        error: (ws: any) => this.conns.delete(ws),
      },
    });
  }

  stop(): void {
    try {
      this.server?.stop(true);
    } catch {
      /* ignore */
    }
    for (const ws of this.conns.keys()) {
      try {
        ws.close();
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
  }

  private async dispatch(ws: any, line: string): Promise<void> {
    let req: RpcRequest;
    try {
      req = JSON.parse(line) as RpcRequest;
    } catch {
      ws.write(JSON.stringify({ id: null, ok: false, error: "invalid JSON" }) + "\n");
      return;
    }
    try {
      const result = this.handle(req.method, req.params ?? {});
      ws.write(JSON.stringify({ id: req.id ?? null, ok: true, result }) + "\n");
    } catch (err) {
      ws.write(JSON.stringify({ id: req.id ?? null, ok: false, error: (err as Error).message }) + "\n");
    }
  }

  private reconcile(): { dead: string[]; adopted: string[] } {
    const live = new Set(this.cp.listSessions().map((s) => s.name));
    const result = this.registry.reconcile(live);
    if (result.dead.length || result.adopted.length) this.registry.save();
    return result;
  }

  private require(id: string): SessionRecord {
    const record = this.registry.get(id);
    if (!record) throw new Error(`unknown session: ${id}`);
    return record;
  }

  private handle(method: string, params: Record<string, unknown>): unknown {
    switch (method) {
      case "ping":
        return { pong: true, version: 1, tmuxSocket: this.cp.socket };

      case "list": {
        this.reconcile();
        return { sessions: this.registry.list() };
      }

      case "new": {
        const kind = (params.kind as SessionKind) ?? "shell";
        const cwd = String(params.cwd ?? process.cwd());
        if (!existsSync(cwd) || !statSync(cwd).isDirectory()) throw new Error(`not a directory: ${cwd}`);
        const id = shortId();
        const tmuxName = `ldmux-${id}`;
        const command =
          kind === "pi" ? ["pi", ...((params.args as string[]) ?? [])] : ((params.command as string[]) ?? []);
        const now = Date.now();
        const record: SessionRecord = {
          id,
          tmuxName,
          kind,
          cwd,
          title: String(params.title ?? (kind === "pi" ? "pi" : "shell")),
          createdAt: now,
          updatedAt: now,
          createdBy: String(params.createdBy ?? "local"),
          status: "running",
        };
        this.cp.newSession({ name: tmuxName, cwd, command, ...this.canonical });
        this.registry.upsert(record);
        this.registry.save();
        return { session: record };
      }

      case "send": {
        const record = this.require(String(params.id));
        if (typeof params.text === "string") this.cp.sendText(record.tmuxName, params.text);
        const keys = (params.keys as string[]) ?? [];
        if (keys.length) this.cp.sendKey(record.tmuxName, ...keys);
        return { ok: true };
      }

      case "capture":
        return { screen: this.cp.capturePane(this.require(String(params.id)).tmuxName, { history: Boolean(params.history) }) };

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

      case "adopt": {
        const tmuxName = String(params.tmuxName);
        if (!this.cp.hasSession(tmuxName)) throw new Error(`no such tmux session: ${tmuxName}`);
        const existing = this.registry.byTmuxName(tmuxName);
        if (existing) return { session: existing };
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
        return { session: record };
      }

      default:
        throw new Error(`unknown method: ${method}`);
    }
  }
}