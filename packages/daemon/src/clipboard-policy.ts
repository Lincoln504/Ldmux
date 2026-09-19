// Whether a program in a shell may put text on the phone's clipboard.
//
// This is a security boundary, so it lives in a file the host owns rather than in
// the client: the pane is arbitrary code, the clipboard is shared with every other
// app on the device, and "which programs may write it" is not a question a phone
// should answer about someone else's machine. The daemon reads it per stream, so
// turning it on does not require restarting a daemon that is holding live shells —
// which on this machine would kill sessions the user is working in.
//
// The default is closed. A missing file means closed, an unreadable file means
// closed, and anything that is not an explicit "on" means closed: a policy that
// fails open is not a policy, and this one is a fail-closed control (see
// docs/SECURITY-CONTROL-REGISTER.md).
import { existsSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

/** The file, inside the daemon's state directory. */
export const CLIPBOARD_WRITE_FILE = "clipboard-write";

export function clipboardWritePolicyPath(stateDir: string): string {
  return join(stateDir, CLIPBOARD_WRITE_FILE);
}

/**
 * Read the policy. Absent, unreadable, or any value other than `on` is closed.
 */
export function readClipboardWritePolicy(stateDir: string): boolean {
  const path = clipboardWritePolicyPath(stateDir);
  if (!existsSync(path)) return false;
  try {
    return readFileSync(path, "utf8").trim().toLowerCase() === "on";
  } catch {
    return false;
  }
}

/** Set the policy. Written atomically enough for a value a daemon re-reads. */
export function writeClipboardWritePolicy(stateDir: string, on: boolean): void {
  writeFileSync(clipboardWritePolicyPath(stateDir), on ? "on\n" : "off\n", {
    mode: 0o600,
  });
}
