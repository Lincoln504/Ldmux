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

/**
 * The tmux server ldmux drives. **Never the user's.**
 *
 * ldmux applies a global option profile (status off, mouse on, history-limit,
 * default-shell, default-terminal, window-size, COLORTERM) and it is only safe
 * because it lands on a server of its own. Pointing it at the user's server —
 * which is the plausible mistake if you want ldmux to "see your existing
 * sessions" — would rewrite their global options under them, and `status off`
 * alone would take away their status bar. So `default` is refused rather than
 * obeyed: the mistake is plausible and the damage is silent.
 */
export function resolveTmuxSocket(): string {
  const name = process.env.LDMUX_TMUX_SOCK ?? "ldmux";
  if (name === "default") {
    throw new Error(
      "LDMUX_TMUX_SOCK=default refused: ldmux must use its own tmux server. " +
        "It sets global tmux options (status, mouse, history-limit, COLORTERM) " +
        "which would change the behaviour of your own tmux sessions.",
    );
  }
  return name;
}