/**
 * Single source of truth for ldmux runtime paths. Every consumer (daemon,
 * CLI, pi extension) resolves state/socket/tmux-socket through these helpers so
 * the fallback chain can never drift between components.
 */

export function resolveStateDir(): string {
  return process.env.LDMUX_STATE_DIR ?? `${process.env.XDG_RUNTIME_DIR ?? "/tmp"}/ldmux`;
}

export function resolveSocketPath(): string {
  return process.env.LDMUX_SOCK ?? `${resolveStateDir()}/ldmuxd.sock`;
}

export function resolveTmuxSocket(): string {
  return process.env.LDMUX_TMUX_SOCK ?? "ldmux";
}