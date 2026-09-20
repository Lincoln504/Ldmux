import { test, expect } from "../../testing/src/expect.ts";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
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
test("reconcile(null) treats tmux state as unknown and marks nothing dead", () => {
  const reg = new Registry(join(dir, "unknown.json"));
  const live = record({ id: "term", tmuxName: "ldmux-x", status: "running" });
  reg.upsert(live);
  const res = reg.reconcile(null);
  expect(res).toEqual({ dead: [], adopted: [] });
  expect(reg.get("term")!.status).toBe("running");
});

test("remove() drops exactly one record and leaves the rest", () => {
  const reg = new Registry(join(dir, "remove.json"));
  reg.upsert(record({ id: "a" }));
  reg.upsert(record({ id: "b" }));
  reg.remove("a");
  expect(reg.get("a")).toBeUndefined();
  expect(reg.get("b")).toBeDefined();
  reg.remove("nope"); // no throw
  expect(reg.list().length).toBe(1);
});

test("a registry written by an older version (with a settings map) still loads", () => {
  const path = join(dir, "legacy.json");
  writeFileSync(
    path,
    JSON.stringify({ version: 1, sessions: [record({ id: "legacy" })], settings: { whatever: true } }),
  );
  const reg = new Registry(path);
  reg.load();
  expect(reg.get("legacy")).toBeDefined();
  reg.save();
  // …and the dead field is not carried forward.
  expect(JSON.parse(readFileSync(path, "utf8")).settings).toBeUndefined();
});

test("load() starts empty on a missing or corrupt file, never throws", () => {
  const missing = new Registry(join(dir, "does-not-exist.json"));
  expect(() => missing.load()).not.toThrow();
  expect(missing.list()).toEqual([]);

  const corrupt = join(dir, "corrupt.json");
  writeFileSync(corrupt, "{ this is not json");
  const r = new Registry(corrupt);
  expect(() => r.load()).not.toThrow();
  expect(r.list()).toEqual([]);
});

test("reconcile revives a dead record tmux still has, and reports orphans", () => {
  const r = new Registry(join(dir, "reconcile.json"));
  r.upsert(record({ id: "revive", tmuxName: "tmux-revive", status: "dead", exitedAt: 5 }));
  // A session tmux has but the registry does not is an adoption candidate.
  const { dead, adopted } = r.reconcile(new Set(["tmux-revive", "tmux-orphan"]));
  expect(dead).toEqual([]);
  expect(adopted).toEqual(["tmux-orphan"]);
  const revived = r.list().find((s) => s.id === "revive")!;
  expect(revived.status).toBe("running");
  expect(revived.exitedAt).toBeNull();
});

test("a malformed record does not take the whole list down", async () => {
  // `registry.json` is loaded without validation — a boundary the daemon's own
  // comment already admits. `sessionTitle` calls `basename(record.cwd)`, and
  // `basename(undefined)` throws `ERR_INVALID_ARG_TYPE`. The throw escaped
  // `describeSessions`, so `list` and every pushed session update failed together:
  // the launcher showed nothing, and every call site ends in `.catch(() => {})`, so
  // nothing anywhere said why. A record that cannot be named is not one that can be
  // shown, so it is dropped where the untrusted bytes arrive.
  const path = join(dir, "malformed-records.json");
  writeFileSync(
    path,
    JSON.stringify({
      version: 1,
      sessions: [
        record({ id: "good", cwd: "/home/me/proj" }),
        { kind: "shell", cwd: "/x" }, // no id
        { id: "no-cwd", kind: "shell", title: "t" }, // id but no cwd
        { id: "", kind: "shell", cwd: "/y" }, // empty id
        null,
      ],
    }),
  );
  const reg = new Registry(path);
  reg.load();
  // Only the usable one survives. A record with no directory is not merely ugly: it
  // used to be kept with `cwd: ""`, and `resolve("")` is `process.cwd()` — so it
  // aliased the caller's own directory, and `ldmux shell`'s reuse rule matched it.
  // `$mod+Return` runs from the home directory, so the phantom won by default.
  expect(reg.list().map((r) => r.id)).toEqual(["good"]);
  // And the survivor is loadable, which is the call that used to throw out of the
  // whole list and blank the launcher.
  expect(basename(reg.get("good")!.cwd)).toBe("proj");
});
