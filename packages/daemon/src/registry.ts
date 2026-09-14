/**
 * Session registry. Crash-safe and restart-safe:
 *   - tmux is authoritative for terminal sessions; the registry caches metadata
 *     tmux cannot hold (kind, title, created-by, pi session id);
 *   - reconcile() marks vanished tmux-backed sessions dead and reports orphans.
 *
 * JSON (not SQLite) for the single-user v1 — see docs/SPEC.md §5.3. Types come
 * from @ldmux/protocol so the daemon, CLI, and extension share one contract.
 */

import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import type { SessionKind, SessionRecord, SessionStatus } from "../../protocol/src/rpc.ts";

export type { SessionKind, SessionRecord, SessionStatus };

interface RegistryFile {
  version: 1;
  sessions: SessionRecord[];
  settings: Record<string, unknown>;
}

export class Registry {
  private sessions = new Map<string, SessionRecord>();
  private settings: Record<string, unknown> = {};

  constructor(private readonly path: string) {}

  load(): void {
    try {
      const raw = readFileSync(this.path, "utf8");
      const data = JSON.parse(raw) as RegistryFile;
      for (const record of data.sessions ?? []) this.sessions.set(record.id, record);
      this.settings = data.settings ?? {};
    } catch {
      // Missing/corrupt registry: start empty (tmux will be re-adopted).
    }
  }

  save(): void {
    mkdirSync(dirname(this.path), { recursive: true });
    const data: RegistryFile = { version: 1, sessions: [...this.sessions.values()], settings: this.settings };
    const tmp = `${this.path}.tmp`;
    writeFileSync(tmp, JSON.stringify(data, null, 2));
    renameSync(tmp, this.path); // atomic
  }

  get(id: string): SessionRecord | undefined {
    return this.sessions.get(id);
  }

  byTmuxName(name: string): SessionRecord | undefined {
    if (!name) return undefined;
    for (const r of this.sessions.values()) if (r.tmuxName === name) return r;
    return undefined;
  }

  byPiSessionId(piSessionId: string): SessionRecord | undefined {
    if (!piSessionId) return undefined;
    for (const r of this.sessions.values()) if (r.piSessionId === piSessionId) return r;
    return undefined;
  }

  list(): SessionRecord[] {
    return [...this.sessions.values()].sort((a, b) => b.updatedAt - a.updatedAt);
  }

  upsert(record: SessionRecord): void {
    this.sessions.set(record.id, record);
  }

  remove(id: string): void {
    this.sessions.delete(id);
  }

  /**
   * Bound registry growth: drop dead records older than `maxDeadAgeMs`, then
   * trim the oldest records if still over `maxRecords`. Returns removed ids.
   */
  prune(opts: { maxDeadAgeMs?: number; maxRecords?: number } = {}, now = Date.now()): string[] {
    const maxDeadAgeMs = opts.maxDeadAgeMs ?? 24 * 60 * 60 * 1000;
    const maxRecords = opts.maxRecords ?? 500;
    const removed: string[] = [];
    for (const record of this.sessions.values()) {
      if (record.status === "dead" && record.exitedAt != null && now - record.exitedAt > maxDeadAgeMs) {
        this.sessions.delete(record.id);
        removed.push(record.id);
      }
    }
    if (this.sessions.size > maxRecords) {
      const oldest = [...this.sessions.values()].sort((a, b) => a.updatedAt - b.updatedAt);
      for (const record of oldest.slice(0, this.sessions.size - maxRecords)) {
        this.sessions.delete(record.id);
        removed.push(record.id);
      }
    }
    return removed;
  }

  /**
   * Reconcile terminal-backed records against the live tmux session set.
   * Records with no tmuxName (RPC-only pi sessions) are left untouched.
   *
   * @returns ids marked dead and live tmux sessions unknown to the registry.
   */
  reconcile(liveTmuxNames: Set<string>, now = Date.now()): { dead: string[]; adopted: string[] } {
    const dead: string[] = [];
    const adopted: string[] = [];
    const known = new Set<string>();
    for (const record of this.sessions.values()) {
      if (!record.tmuxName) continue; // RPC-only; liveness is the pi process
      known.add(record.tmuxName);
      if (liveTmuxNames.has(record.tmuxName)) {
        if (record.status !== "running") {
          record.status = "running";
          record.exitedAt = null;
          record.exitCode = null;
          record.updatedAt = now;
        }
      } else if (record.status !== "dead") {
        record.status = "dead";
        record.exitedAt = record.exitedAt ?? now;
        record.updatedAt = now;
        dead.push(record.id);
      }
    }
    for (const name of liveTmuxNames) {
      if (!known.has(name)) adopted.push(name);
    }
    return { dead, adopted };
  }
}