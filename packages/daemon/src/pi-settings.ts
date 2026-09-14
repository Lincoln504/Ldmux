/**
 * Editing pi's `settings.json` safely.
 *
 * ldmux must be loaded by pi from an **absolute real path** (docs/PI-EXTENSIONS.md):
 * a symlink into `~/.pi/agent/extensions/` breaks the buildless relative imports
 * because pi's jiti resolves them against the symlink target. So the install
 * step is a small, careful edit of a user-owned config file, which is exactly
 * the sort of thing that should be pure, tested, and backed up.
 *
 * Everything here is a pure function over plain data; the CLI does the I/O.
 */

export const EXTENSION_SETTING_KEY = "extensions";

export interface SettingsEdit {
  settings: Record<string, unknown>;
  changed: boolean;
  /** Human-readable summary of what happened. */
  note: string;
}

function asObject(value: unknown): Record<string, unknown> | null {
  return value && typeof value === "object" && !Array.isArray(value) ? (value as Record<string, unknown>) : null;
}

/** Read the `extensions` array, ignoring anything malformed. */
export function extensionEntries(settings: unknown): string[] {
  const obj = asObject(settings);
  const value = obj?.[EXTENSION_SETTING_KEY];
  if (!Array.isArray(value)) return [];
  return value.filter((entry): entry is string => typeof entry === "string");
}

function withEntries(settings: Record<string, unknown>, entries: string[]): Record<string, unknown> {
  return { ...settings, [EXTENSION_SETTING_KEY]: entries };
}

/**
 * Add `extensionPath` to `pi.extensions`, preserving every other key and the
 * order of existing entries. Idempotent: adding a path that is already present
 * reports `changed: false` so the caller can skip the backup+write entirely.
 *
 * `extensionPath` must be absolute: a relative entry would resolve against
 * whatever directory pi happened to start in.
 */
export function addExtension(settings: unknown, extensionPath: string): SettingsEdit {
  if (!extensionPath.startsWith("/")) {
    throw new Error(`extension path must be absolute: ${extensionPath}`);
  }
  const obj = asObject(settings) ?? {};
  const entries = extensionEntries(settings);
  if (entries.includes(extensionPath)) {
    return { settings: obj, changed: false, note: "already present" };
  }
  return {
    settings: withEntries(obj, [...entries, extensionPath]),
    changed: true,
    note: `added ${extensionPath}`,
  };
}

/** Remove `extensionPath`; never throws, and leaves other entries untouched. */
export function removeExtension(settings: unknown, extensionPath: string): SettingsEdit {
  const obj = asObject(settings) ?? {};
  const entries = extensionEntries(settings);
  if (!entries.includes(extensionPath)) {
    return { settings: obj, changed: false, note: "not present" };
  }
  return {
    settings: withEntries(
      obj,
      entries.filter((entry) => entry !== extensionPath),
    ),
    changed: true,
    note: `removed ${extensionPath}`,
  };
}

/** Where pi keeps its settings, unless the environment says otherwise. */
export function resolveSettingsPath(env: Record<string, string | undefined> = process.env, home = process.env.HOME ?? ""): string {
  if (env.LDMUX_PI_SETTINGS) return env.LDMUX_PI_SETTINGS;
  return `${home}/.pi/agent/settings.json`;
}

/**
 * A previous version of this project recommended symlinking the extension into
 * pi's discovery directory. That silently breaks ldmux, so detect it and warn
 * instead of leaving the user with an extension that fails to load.
 */
export function detectLegacySymlink(home: string, settings: unknown): string | null {
  const legacy = `${home}/.pi/agent/extensions/ldmux`;
  const entries = extensionEntries(settings);
  if (entries.some((entry) => entry.endsWith("/extensions/ldmux") || entry === legacy)) {
    return `settings still point at the discovery directory (${legacy}); remove that symlink`;
  }
  return null;
}