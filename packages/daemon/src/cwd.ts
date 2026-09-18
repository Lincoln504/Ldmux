import { homedir } from "node:os";
import { join } from "node:path";

/**
 * Where a session's directory comes from, in one place.
 *
 * Two things were wrong with asking the filesystem directly.
 *
 * `~` was refused. It is how every person writes their home directory, on a phone
 * keyboard more than anywhere, and answering `not a directory: ~` to a path that
 * plainly names one refuses the request rather than the mistake.
 *
 * And an absent directory meant `process.cwd()`, the daemon's own working directory,
 * which the person asking cannot see and did not choose. A home directory is at least
 * a directory somebody can name.
 *
 * Only a leading `~` is expanded, and only when it is the whole path or is followed by
 * a separator, so a directory genuinely called `~foo` is still itself. No shell is
 * involved and none is spawned: this is string arithmetic, not an `eval`.
 *
 * Exported and tested on its own because the interesting part is the edge cases, and
 * they are cheap to test here and expensive to test through a tmux session.
 */
export function resolveCwd(cwd: unknown): string {
  const raw = typeof cwd === "string" && cwd.trim() !== "" ? cwd.trim() : homedir();
  if (raw === "~") return homedir();
  if (raw.startsWith("~/")) return join(homedir(), raw.slice(2));
  return raw;
}
