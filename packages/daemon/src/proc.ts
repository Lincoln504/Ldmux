/**
 * Running an external process, async, with both pipes drained concurrently.
 *
 * Two properties matter and both are easy to get wrong:
 *
 * - **Asynchronous.** The daemon has one event loop shared by every WebSocket
 *   reader; a synchronous spawn blocks all of them for the lifetime of the
 *   process.
 * - **Both pipes drained at once.** Reading stdout to completion before touching
 *   stderr deadlocks as soon as the child fills the one we are not reading.
 *
 * The `env` is passed explicitly rather than left to the default: a child must
 * see the environment as it is *now*, so a PATH changed at runtime (a stubbed
 * `tmux` in tests, or a user's PATH fix) is honoured instead of a snapshot taken
 * at process start.
 */

import { spawn, type ChildProcess } from "node:child_process";
import type { Readable } from "node:stream";

export interface SpawnedProcess {
  /**
   * Terminate the child **and its descendants** with the given signal number
   * (default SIGTERM).
   *
   * The group matters: a wedged `tmux` may have started helpers of its own, and
   * signalling only the process we know about leaves those helpers holding the
   * pipes open — so the caller's read never ends. That is not hypothetical: it
   * turned a 300 ms timeout into a 300-second leak.
   */
  kill(signal?: number): void;
  /** Exit code, or -1 when killed by a signal or when spawn itself failed. */
  exited: Promise<number>;
  /** Both pipes, drained as text, concurrently. */
  drained: Promise<{ out: string; err: string }>;
  /** The spawn failure, if there was one (e.g. the binary does not exist). */
  spawnError: Promise<Error | null>;
}

export interface SpawnOptions {
  env?: NodeJS.ProcessEnv;
  /** Stdin is always ignored: these children are never interactive. */
  cwd?: string;
}

export function spawnCaptured(argv: string[], opts: SpawnOptions = {}): SpawnedProcess {
  let child: ChildProcess;
  let spawnFailure: Error | null = null;
  let resolveSpawnError: (err: Error | null) => void = () => {};
  const spawnError = new Promise<Error | null>((resolve) => {
    resolveSpawnError = resolve;
  });

  try {
    child = spawn(argv[0]!, argv.slice(1), {
      stdio: ["ignore", "pipe", "pipe"],
      env: opts.env ?? process.env,
      ...(opts.cwd ? { cwd: opts.cwd } : {}),
      // Its own process group, so kill() can take the whole tree down.
      detached: true,
    });
  } catch (err) {
    // Only thrown for a bad argument list (Node reports ENOENT asynchronously).
    spawnFailure = err as Error;
    resolveSpawnError(spawnFailure);
    return {
      kill: () => {},
      exited: Promise.resolve(-1),
      drained: Promise.resolve({ out: "", err: String((err as Error).message) }),
      spawnError,
    };
  }

  const exited = new Promise<number>((resolve) => {
    child.once("error", (err: Error) => {
      spawnFailure = err;
      resolveSpawnError(err);
      resolve(-1);
    });
    child.once("close", (code, signal) => {
      resolveSpawnError(null);
      // A signal-terminated child has no code; -1 keeps the "did not exit
      // normally" distinction the callers already make.
      resolve(signal ? -1 : (code ?? -1));
    });
  });

  const drained = Promise.all([
    readAll(child.stdout),
    readAll(child.stderr),
  ]).then(([out, err]) => ({ out, err }));

  return {
    kill: (signal?: number) => killTree(child, signal),
    exited,
    drained,
    spawnError,
  };
}

/**
 * Signal the child's whole process group, falling back to the child alone.
 *
 * A negative pid means "the group": the child was started detached, so its pgid
 * is its own pid and every descendant is reached by one call.
 *
 * Two caveats are inherent to the technique and accepted: the child now leads
 * its own session, so it outlives a SIGKILL of *this* process (tmux servers are
 * meant to), and a pid that has already been reaped could in principle be
 * recycled before the signal lands. Callers only kill children they still hold.
 */
export function killTree(child: ChildProcess, signal?: number): void {
  const pid = child.pid;
  try {
    if (pid !== undefined && process.platform !== "win32") {
      process.kill(-pid, signal ?? "SIGTERM");
      return;
    }
  } catch {
    /* the group is already gone, or pid is unset; fall through */
  }
  try {
    if (signal === undefined) child.kill();
    else child.kill(signal);
  } catch {
    /* already gone */
  }
}

/** Collect a stream as UTF-8 text. Never rejects: a broken pipe is not fatal. */
function readAll(stream: Readable | null): Promise<string> {
  if (!stream) return Promise.resolve("");
  return new Promise<string>((resolve) => {
    const chunks: Buffer[] = [];
    stream.on("data", (chunk: Buffer) => chunks.push(chunk));
    stream.on("end", () => resolve(Buffer.concat(chunks).toString("utf8")));
    // A child killed mid-write ends the stream with an error we do not care
    // about; resolve with what arrived rather than rejecting.
    stream.on("error", () => resolve(Buffer.concat(chunks).toString("utf8")));
  });
}
