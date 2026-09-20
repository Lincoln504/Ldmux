/**
 * Test hygiene: reap the tmux server a test started.
 *
 * `tmux -L <name> kill-server` exiting non-zero is not enough — when the server
 * has already exited on its own the *socket file* remains, and test runs
 * accumulate hundreds of them in tmux's socket directory. So: kill, then unlink
 * every path tmux could have used for that socket name.
 *
 * tmux resolves a socket under `${TMUX_TMPDIR:-/tmp}/tmux-<uid>/`, **not** under
 * `os.tmpdir()` (inside `nix develop` TMPDIR is a nix-shell directory, which is
 * why an earlier version of this helper silently deleted nothing), and if
 * TMUX_TMPDIR is itself `/tmp/tmux-<uid>` tmux nests one level deeper.
 */
import { rmSync } from "node:fs";
import { join } from "node:path";
import { spawnSync } from "node:child_process";

export function killTmuxServer(socket: string): void {
  try {
    spawnSync("tmux", ["-L", socket, "kill-server"], { stdio: "ignore" });
  } catch {
    /* no tmux on PATH */
  }
  const uid = typeof process.getuid === "function" ? process.getuid() : 0;
  const dir = `tmux-${uid}`;
  // tmux resolves a socket under `${TMUX_TMPDIR:-${TMPDIR:-/tmp}}/tmux-<uid>/`. This
  // read `TMUX_TMPDIR` alone, while the docstring above names `TMPDIR` as the variable
  // that matters inside `nix develop` — so with `TMUX_TMPDIR` unset, which is the normal
  // case, it unlinked under `/tmp` while tmux had written to the nix-shell TMPDIR. The
  // sockets this exists to reap went on accumulating, in exactly the environment the
  // comment says it was written for.
  //
  // Every base is enumerated rather than one base plus suffixes. With `base === "/tmp"`
  // the old third candidate was identical to the second, so `new Set` silently collapsed
  // two of the three paths it looked like it was checking.
  const bases = [process.env.TMUX_TMPDIR, process.env.TMPDIR, "/tmp"].filter(
    (b): b is string => typeof b === "string" && b !== "",
  );
  const candidates = bases.flatMap((base) => [join(base, dir), join(base, dir, dir)]);
  for (const candidate of new Set(candidates)) {
    try {
      rmSync(join(candidate, socket), { force: true });
    } catch {
      /* nothing to unlink */
    }
  }
}