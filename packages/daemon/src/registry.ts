/**
 * Session registry. Crash-safe and restart-safe:
 *   - tmux is authoritative; the registry is a cache of metadata tmux cannot hold
 *     (kind, title, created-by, own id);
 *   - reconcile() adopts surviving tmux sessions and marks vanished ones dead.
 *
 * JSON (not SQLite) for the single-user v1 — see docs/SPEC.md §5.3.
 */

import { mkdirSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

export type SessionKind = "shell" | "pi";
export type SessionStatus = "starting" | "running" | "dead";

export interface SessionRecord {
  id: string;
  tmuxName: string;
  kind: SessionKind;
  cwd: string;
  title: string;
  createdAt: number;
  updatedAt: number;
  createdBy: string;
  status: SessionStatus;
  exitCode?: number | null;
  exitedAt?: number | null;
}

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
    for (const r of this.sessions.values()) if (r.tmuxName === name) return r;
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
   * Reconcile against the live tmux session set.
   * @returns ids marked dead and tmux sessions adopted (unknown to the registry).
   */
  reconcile(liveTmuxNames: Set<string>, now = Date.now()): { dead: string[]; adopted: string[] } {
    const dead: string[] = [];
    const adopted: string[] = [];
    const known = new Set<string>();
    for (const record of this.sessions.values()) {
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