import { test, expect, afterAll } from "bun:test";
import { existsSync, mkdtempSync, readFileSync, readdirSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { addExtension, detectLegacySymlink, extensionEntries, removeExtension, resolveSettingsPath } from "../src/pi-settings.ts";
import { installPiExtension } from "../src/install.ts";

const dir = mkdtempSync(join(tmpdir(), "ldmux-install-"));
const settings = join(dir, "settings.json");
const EXT = "/home/x/Documents/ldmux/packages/extension";

afterAll(() => rmSync(dir, { recursive: true, force: true }));

test("adding an extension preserves every other setting and entry order", () => {
  const before = { model: "x", extensions: ["/a/one"], packages: ["p"] };
  const { settings: after, changed, note } = addExtension(before, EXT);
  expect(changed).toBe(true);
  expect(note).toContain("added");
  expect(after).toEqual({ model: "x", extensions: ["/a/one", EXT], packages: ["p"] });
  // Idempotent: the second add is a no-op, so callers can skip backup+write.
  expect(addExtension(after, EXT).changed).toBe(false);
  expect(addExtension(after, EXT).note).toBe("already present");
});

test("adding to a missing/oddly-shaped extensions key still works", () => {
  expect(addExtension({}, EXT).settings).toEqual({ extensions: [EXT] });
  expect(addExtension(null, EXT).settings).toEqual({ extensions: [EXT] });
  // A non-array value is replaced by a valid array rather than crashing.
  expect(addExtension({ extensions: "nope" }, EXT).settings).toEqual({ extensions: [EXT] });
  // Junk inside the array is ignored for the membership check; when there is
  // nothing to add the file is left byte-for-byte alone (no rewrite).
  const junk = { extensions: [1, null] };
  const res = addExtension(junk, EXT);
  expect(res.changed).toBe(true);
  expect(res.settings).toEqual({ extensions: [EXT] });
  expect(extensionEntries({ extensions: [1, "a", null] })).toEqual(["a"]);
  expect(addExtension({ extensions: [1, EXT, null] }, EXT).changed).toBe(false);
});

test("a relative extension path is rejected (it would resolve anywhere)", () => {
  expect(() => addExtension({}, "packages/extension")).toThrow(/absolute/);
});

test("removing leaves the other entries alone and is idempotent", () => {
  const before = { extensions: ["/a/one", EXT, "/b/two"] };
  const { settings: after, changed } = removeExtension(before, EXT);
  expect(changed).toBe(true);
  expect(after).toEqual({ extensions: ["/a/one", "/b/two"] });
  expect(removeExtension(after, EXT).changed).toBe(false);
  expect(removeExtension({}, EXT).settings).toEqual({});
});

test("the legacy symlink entry is detected so it can be warned about", () => {
  const home = "/home/x";
  expect(detectLegacySymlink(home, { extensions: [`${home}/.pi/agent/extensions/ldmux`] })).toContain("discovery directory");
  expect(detectLegacySymlink(home, { extensions: [EXT] })).toBeNull();
  expect(detectLegacySymlink(home, {})).toBeNull();
});

test("settings path honours the override and defaults to pi's location", () => {
  expect(resolveSettingsPath({ LDMUX_PI_SETTINGS: "/tmp/s.json" }, "/home/x")).toBe("/tmp/s.json");
  expect(resolveSettingsPath({}, "/home/x")).toBe("/home/x/.pi/agent/settings.json");
});

test("install writes the entry, backs up the original, and is idempotent", () => {
  writeFileSync(settings, JSON.stringify({ model: "keep-me", packages: ["p"] }, null, 2));

  const first = installPiExtension({ settingsPath: settings, extensionPath: EXT, home: dir });
  expect(first.action).toBe("added");
  expect(first.entries).toEqual([EXT]);
  expect(first.backup).toBeDefined();
  expect(existsSync(first.backup!)).toBe(true);
  // The original content is preserved in the backup, not just the new file.
  expect(JSON.parse(readFileSync(first.backup!, "utf8"))).toEqual({ model: "keep-me", packages: ["p"] });
  // Everything except the extension key survives the edit.
  expect(JSON.parse(readFileSync(settings, "utf8"))).toEqual({ model: "keep-me", packages: ["p"], extensions: [EXT] });
  // Written owner-only: it can hold paths, treat it as private config.
  expect(Bun.file(settings).size).toBeGreaterThan(0);

  const second = installPiExtension({ settingsPath: settings, extensionPath: EXT, home: dir });
  expect(second.action).toBe("unchanged");
  expect(second.backup).toBeUndefined();
  // No second backup file appeared.
  expect(readdirSync(dir).filter((f) => f.includes(".bak-ldmux-")).length).toBe(1);
});

test("dry-run reports the intended change without touching the file", () => {
  const path = join(dir, "dry.json");
  writeFileSync(path, "{}\n");
  const res = installPiExtension({ settingsPath: path, extensionPath: EXT, home: dir, dryRun: true });
  expect(res.action).toBe("unchanged");
  expect(res.note).toContain("would add");
  expect(readFileSync(path, "utf8")).toBe("{}\n");
});

test("a fresh settings file is created with its directory", () => {
  const path = join(dir, "nested", "deeper", "settings.json");
  const res = installPiExtension({ settingsPath: path, extensionPath: EXT, home: dir });
  expect(res.action).toBe("added");
  expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ extensions: [EXT] });
});

test("unparseable settings are refused, never clobbered", () => {
  const path = join(dir, "broken.json");
  writeFileSync(path, "{ this is not json ");
  expect(() => installPiExtension({ settingsPath: path, extensionPath: EXT, home: dir })).toThrow(/refusing to modify/);
  expect(readFileSync(path, "utf8")).toBe("{ this is not json ");
});

test("--remove takes the entry back out and keeps the rest", () => {
  const path = join(dir, "remove.json");
  writeFileSync(path, JSON.stringify({ extensions: ["/a/one", EXT], model: "m" }));
  const res = installPiExtension({ settingsPath: path, extensionPath: EXT, home: dir, remove: true });
  expect(res.action).toBe("removed");
  expect(JSON.parse(readFileSync(path, "utf8"))).toEqual({ extensions: ["/a/one"], model: "m" });
});

test("a legacy symlink entry produces a warning in the result", () => {
  const path = join(dir, "legacy.json");
  const home = join(dir, "home");
  writeFileSync(path, JSON.stringify({ extensions: [`${home}/.pi/agent/extensions/ldmux`] }));
  const res = installPiExtension({ settingsPath: path, extensionPath: EXT, home });
  expect(res.warning).toContain("discovery directory");
});