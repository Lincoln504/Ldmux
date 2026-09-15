/**
 * Running a short-lived external command from the CLI.
 *
 * Same discipline as the tmux control plane: asynchronous (a CLI that blocks its
 * event loop cannot be tested against an in-process daemon), both pipes drained
 * concurrently (a full pipe would otherwise deadlock), and bounded so a wedged
 * binary cannot hang the command forever.
 */

import { spawnCaptured } from "./proc.ts";

export interface ExecResult {
  code: number;
  out: string;
  err: string;
}

export const EXEC_TIMEOUT_MS = 10_000;
/** How long a command may ignore SIGTERM before it is killed outright. */
export const EXEC_KILL_GRACE_MS = 500;

export async function run(
  argv: string[],
  opts: { timeoutMs?: number; env?: Record<string, string | undefined> } = {},
): Promise<ExecResult> {
  const timeoutMs = opts.timeoutMs ?? EXEC_TIMEOUT_MS;
  // The caller's environment (else the live one): a child must not see a
  // snapshot taken at process start, or a PATH changed later is silently
  // ignored — which once made tests run the real binary instead of the stub.
  const proc = spawnCaptured(argv, { env: (opts.env ?? process.env) as NodeJS.ProcessEnv });

  let timedOut = false;
  // Escalate, like the tmux control plane does: SIGTERM, then SIGKILL. A
  // SIGTERM alone is not a bound — a binary that traps or ignores it would hang
  // this call (and the daemon's event loop work behind it) forever, which is
  // exactly what this function promises not to do.
  const termTimer = setTimeout(() => {
    timedOut = true;
    proc.kill();
  }, timeoutMs);
  const killTimer = setTimeout(() => proc.kill(9), timeoutMs + EXEC_KILL_GRACE_MS);

  try {
    const failure = await proc.spawnError;
    if (failure) {
      // Missing binary, permission denied, …
      return { code: -1, out: "", err: failure.message };
    }
    const [{ out, err }, code] = await Promise.all([proc.drained, proc.exited]);
    if (timedOut) return { code: -1, out, err: `command timed out after ${timeoutMs}ms` };
    return { code, out, err };
  } finally {
    clearTimeout(termTimer);
    clearTimeout(killTimer);
  }
}
