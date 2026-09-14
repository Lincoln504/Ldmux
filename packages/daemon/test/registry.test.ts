import { test, expect } from "bun:test";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Registry } from "../src/registry.ts";
import type { SessionRecord } from "../../protocol/src/rpc.ts";

const dir = mkdtempSync(join(tmpdir(), "ldmux-registry-"));

function record(over: Partial<SessionRecord>): SessionRecord {
  const now = Date.now();
  return {
    id: over.id ?? Math.random().toString(36).slice(2),
    tmuxName: "",
    kind: "shell",
    cwd: "/tmp",
    title: "t",
    createdAt: now,
    updatedAt: now,
    createdBy: "test",
    status: "running",
    ...over,
  };
}

test("prune drops dead records older than the TTL", () => {
  const reg = new Registry(join(dir, "a.json"));
  const old = record({ id: "old", status: "dead", exitedAt: Date.now() - 60_000, updatedAt: Date.now() - 60_000 });
  const fresh = record({ id: "fresh", status: "dead", exitedAt: Date.now() });
  const live = record({ id: "live", status: "running" });
  reg.upsert(old);
  reg.upsert(fresh);
  reg.upsert(live);
  const removed = reg.prune({ maxDeadAgeMs: 30_000 });
  expect(removed).toEqual(["old"]);
  expect(reg.get("old")).toBeUndefined();
  expect(reg.get("fresh")).toBeDefined();
  expect(reg.get("live")).toBeDefined();
});

test("prune enforces a hard record cap by oldest-updated", () => {
  const reg = new Registry(join(dir, "b.json"));
  for (let i = 0; i < 10; i++) reg.upsert(record({ id: `s${i}`, updatedAt: i }));
  const removed = reg.prune({ maxRecords: 4 });
  expect(removed.sort()).toEqual(["s0", "s1", "s2", "s3", "s4", "s5"]);
  expect(reg.list()).toHaveLength(4);
});

test("reconcile marks vanished tmux sessions dead but leaves RPC-only ones", () => {
  const reg = new Registry(join(dir, "c.json"));
  reg.upsert(record({ id: "term", tmuxName: "ldmux-x", status: "running" }));
  reg.upsert(record({ id: "rpc", tmuxName: "", status: "running" }));
  const { dead } = reg.reconcile(new Set());
  expect(dead).toEqual(["term"]);
  expect(reg.get("term")?.status).toBe("dead");
  expect(reg.get("rpc")?.status).toBe("running");
});

test("save/load round-trips atomically", () => {
  const path = join(dir, "d.json");
  const reg = new Registry(path);
  reg.upsert(record({ id: "persist" }));
  reg.save();
  const reloaded = new Registry(path);
  reloaded.load();
  expect(reloaded.get("persist")?.id).toBe("persist");
  rmSync(path, { force: true });
});