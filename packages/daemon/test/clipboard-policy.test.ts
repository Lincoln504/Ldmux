import { test, expect } from "../../testing/src/expect.ts";
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, readFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  clipboardWritePolicyPath,
  readClipboardWritePolicy,
  writeClipboardWritePolicy,
} from "../src/clipboard-policy.ts";

function withStateDir(fn: (dir: string) => void): void {
  const dir = mkdtempSync(join(tmpdir(), "ldmux-clip-"));
  try {
    fn(dir);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

test("a missing policy file is closed", () => {
  withStateDir((dir) => {
    expect(readClipboardWritePolicy(dir)).toBe(false);
  });
});

test("only an explicit on is open", () => {
  withStateDir((dir) => {
    for (const body of ["off", "no", "false", "", "  \n", "ON?\n", "1", "yes"]) {
      writeFileSync(clipboardWritePolicyPath(dir), body);
      expect(readClipboardWritePolicy(dir)).toBe(false);
    }
    for (const body of ["on", "on\n", "ON", " On \n"]) {
      writeFileSync(clipboardWritePolicyPath(dir), body);
      expect(readClipboardWritePolicy(dir)).toBe(true);
    }
  });
});

test("a directory where the file belongs is closed, not a crash", () => {
  // A fail-closed control must not become an exception that takes the daemon down
  // when someone creates the path wrongly.
  withStateDir((dir) => {
    const path = clipboardWritePolicyPath(dir);
    writeFileSync(path, "on\n");
    // Replace the file with a directory: unreadable as text.
    rmSync(path);
    mkdirSync(path);
    writeFileSync(join(path, "occupied"), "x");
    expect(readClipboardWritePolicy(dir)).toBe(false);
  });
});

test("writing the policy round-trips and is private", () => {
  withStateDir((dir) => {
    writeClipboardWritePolicy(dir, true);
    expect(readClipboardWritePolicy(dir)).toBe(true);
    expect(readFileSync(clipboardWritePolicyPath(dir), "utf8").trim()).toBe("on");

    writeClipboardWritePolicy(dir, false);
    expect(readClipboardWritePolicy(dir)).toBe(false);
    // Off is written, not unlinked: a file that says off is legible, and an
    // absence is indistinguishable from a state dir nobody has configured.
    expect(readFileSync(clipboardWritePolicyPath(dir), "utf8").trim()).toBe("off");
  });
});
