/**
 * Running a short-lived external command from the CLI.
 *
 * Same discipline as the tmux control plane: asynchronous (a CLI that blocks its
 * event loop cannot be tested against an in-process daemon), both pipes drained
 * concurrently (a full pipe would otherwise deadlock), and bounded so a wedged
 * binary cannot hang the command forever.
 */

export interface ExecResult {
  code: number;
  out: string;
  err: string;
}

export const EXEC_TIMEOUT_MS = 10_000;

export async function run(
  argv: string[],
  opts: { timeoutMs?: number; env?: Record<string, string | undefined> } = {},
): Promise<ExecResult> {
  const timeoutMs = opts.timeoutMs ?? EXEC_TIMEOUT_MS;
  let proc: Bun.Subprocess<"ignore", "pipe", "pipe">;
  try {
    // Pass `env` explicitly (the caller's, else the live process env): Bun.spawn
    // without it uses the environment snapshot from process start, so a PATH
    // changed later is ignored. That silently ran the real binary in tests that
    // stub one, and would ignore runtime env changes in the daemon.
    const spawnOpts: Parameters<typeof Bun.spawn>[1] = {
      stdin: "ignore",
      stdout: "pipe",
      stderr: "pipe",
      env: (opts.env ?? process.env) as Record<string, string>,
    };
    proc = Bun.spawn(argv, spawnOpts);
  } catch (err) {
    // Missing binary, permission denied, …
    return { code: -1, out: "", err: (err as Error).message };
  }
  let timedOut = false;
  const timer = setTimeout(() => {
    timedOut = true;
    try {
      proc.kill();
    } catch {
      /* already gone */
    }
  }, timeoutMs);
  try {
    const [out, err, code] = await Promise.all([
      new Response(proc.stdout).text(),
      new Response(proc.stderr).text(),
      proc.exited,
    ]);
    if (timedOut) return { code: -1, out, err: `command timed out after ${timeoutMs}ms` };
    return { code: code ?? -1, out, err };
  } finally {
    clearTimeout(timer);
  }
}
