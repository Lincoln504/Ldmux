/**
 * Installing the ldmux pi extension into pi's settings.
 *
 * The extension must be loaded from an absolute real path (docs/PI-EXTENSIONS.md);
 * symlinking it into pi's discovery directory breaks its buildless relative
 * imports. This module performs the careful part — read, validate, edit, back
 * up, write — and returns a structured result so the CLI stays a thin printer
 * and the behaviour is testable against a temporary settings file.
 */

import { copyFileSync, existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { addExtension, detectLegacySymlink, extensionEntries, removeExtension, resolveSettingsPath } from "./pi-settings.ts";

export interface InstallOptions {
  /** Defaults to resolveSettingsPath() (`$LDMUX_PI_SETTINGS` or ~/.pi/agent/settings.json). */
  settingsPath?: string;
  /** Defaults to the extension package beside this source file. */
  extensionPath?: string;
  home?: string;
  remove?: boolean;
  dryRun?: boolean;
}

export interface InstallResult {
  settingsPath: string;
  extensionPath: string;
  action: "added" | "removed" | "unchanged";
  backup?: string;
  entries: string[];
  warning?: string;
  note: string;
}

function defaultExtensionPath(): string {
  // fileURLToPath, never `.pathname`: the latter percent-encodes, so a checkout
  // under a path with a space would install a path pi cannot load.
  return fileURLToPath(new URL("../../extension", import.meta.url)).replace(/\/$/, "");
}

function readSettings(path: string): unknown {
  if (!existsSync(path)) return {};
  const raw = readFileSync(path, "utf8");
  if (raw.trim() === "") return {};
  let parsed: unknown;
  try {
    parsed = JSON.parse(raw);
  } catch (err) {
    // Never rewrite a file we cannot parse: the user may have a JSONC-ish
    // settings file, and clobbering it would be worse than failing.
    throw new Error(`${path} is not valid JSON (${(err as Error).message}); refusing to modify it`);
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    // Same principle for a file that parses but is not a settings object.
    throw new Error(`${path} is not a JSON object; refusing to modify it`);
  }
  return parsed;
}

/**
 * Add (or remove) the ldmux extension entry, preserving everything else in the
 * file. Writes a timestamped backup before the first modification, and is
 * idempotent: a second run reports `unchanged` and touches nothing.
 */
export function installPiExtension(opts: InstallOptions = {}): InstallResult {
  const home = opts.home ?? process.env.HOME ?? "";
  const settingsPath = opts.settingsPath ?? resolveSettingsPath(process.env, home);
  const extensionPath = (opts.extensionPath ?? defaultExtensionPath()).replace(/\/$/, "");
  const data = readSettings(settingsPath);
  const warning = detectLegacySymlink(home, data) ?? undefined;

  const edit = opts.remove ? removeExtension(data, extensionPath) : addExtension(data, extensionPath);
  const entries = extensionEntries(edit.settings);

  if (!edit.changed || opts.dryRun) {
    return {
      settingsPath,
      extensionPath,
      action: "unchanged",
      entries,
      warning,
      note: opts.dryRun && edit.changed ? `would ${opts.remove ? "remove" : "add"} ${extensionPath}` : edit.note,
    };
  }

  let backup: string | undefined;
  if (existsSync(settingsPath)) {
    backup = `${settingsPath}.bak-ldmux-${new Date().toISOString().replace(/[:.]/g, "-")}`;
    copyFileSync(settingsPath, backup);
  } else {
    mkdirSync(dirname(settingsPath), { recursive: true, mode: 0o700 });
  }
  writeFileSync(settingsPath, JSON.stringify(edit.settings, null, 2) + "\n", { mode: 0o600 });
  return {
    settingsPath,
    extensionPath,
    action: opts.remove ? "removed" : "added",
    backup,
    entries,
    warning,
    note: edit.note,
  };
}
