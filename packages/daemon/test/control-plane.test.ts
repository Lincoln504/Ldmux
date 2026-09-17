import { test, expect, afterAll } from "../../testing/src/expect.ts";
import { chmodSync, existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import {
  tmuxActionArgs,
  isValidSessionName,
  resolveLoginShell,
  TmuxControlPlane,
} from "../src/tmux/control-plane.ts";
import { killTmuxServer } from "./tmux-cleanup.ts";

const dir = mkdtempSync(join(tmpdir(), "ldmux-faketmux-"));
const realPath = process.env.PATH ?? "";

/**
 * These tests spawn real tmux servers; a leaked one outlives the test run, so
 * every file that creates one must reap it even when a test fails or times out.
 */
const SOCKET = `ldmux-cp-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;

afterAll(() => {
  killTmuxServer(SOCKET);
  process.env.PATH = realPath;
  rmSync(dir, { recursive: true, force: true });
});

/**
 * A tmux that ignores SIGTERM and never exits: without the SIGKILL-and-deadline
 * escalation in run(), the caller (and, in the daemon, a whole connection)
 * would hang forever.
 *
 * The stub drops a marker file so a test can prove the stub was the binary that
 * ran. Without that, a test "passes" even when the real tmux is used (a
 * nonexistent socket also yields a null session list), which is exactly the
 * vacuous state this file was in before.
 */
const MARKER = join(dir, "stub-ran");
function installStubbornFakeTmux(): void {
  const script = join(dir, "tmux");
  writeFileSync(script, `#!/bin/sh\ntouch ${MARKER}\ntrap "" TERM\necho started\nsleep 300\n`);
  chmodSync(script, 0o755);
  rmSync(MARKER, { force: true });
  process.env.PATH = `${dir}:${realPath}`;
}

test("a tmux that ignores SIGTERM still returns (bounded, code -1)", async () => {
  installStubbornFakeTmux();
  const cp = new TmuxControlPlane("test-socket", 300);
  const started = Date.now();
  const sessions = await cp.listSessions();
  const elapsed = Date.now() - started;
  // The stub really ran (otherwise this test would silently exercise real tmux).
  expect(existsSync(MARKER)).toBe(true);
  // code -1 must read as "unknown", never as "no sessions".
  expect(sessions).toBeNull();
  // The stub never exits on its own, so returning at all proves the timeout
  // fired; the lower bound proves we waited for it rather than getting a fast
  // failure from a real tmux with no server.
  expect(elapsed).toBeGreaterThanOrEqual(300);
  // 300ms SIGTERM + 150ms grace + 500ms deadline is the worst case.
  expect(elapsed).toBeLessThan(3000);
});

test("hard deadline returns even if the pipes never close", async () => {
  installStubbornFakeTmux();
  const cp = new TmuxControlPlane("test-socket", 100);
  const started = Date.now();
  const sessions = await cp.listSessions();
  expect(existsSync(MARKER)).toBe(true);
  expect(sessions).toBeNull();
  const elapsed = Date.now() - started;
  expect(elapsed).toBeGreaterThanOrEqual(100);
  expect(elapsed).toBeLessThan(2500);
});

test("a normal tmux command is unaffected", async () => {
  process.env.PATH = realPath;
  const cp = new TmuxControlPlane(SOCKET);
  await cp.ensureServer({ historyLimit: 1000 });
  await cp.newSession({ name: "t", cwd: tmpdir(), command: [], cols: 80, rows: 24 });
  const sessions = await cp.listSessions();
  expect(sessions?.map((s) => s.name)).toContain("t");
  expect(await cp.hasSession("t")).toBe(true);
  expect(await cp.hasSession("nope")).toBe(false);
  await cp.killSession("t");
});
test("isValidSessionName is the argv-safety boundary, tested directly", () => {
  // These names reach `tmux ... -t <name>` and are stored on a record; the
  // regex must reject anything that could be read as a flag or a shell token.
  for (const bad of ["", "a b", "a;b", "a|b", "$(id)", "a/b", "a.b", "a\nb", "a".repeat(129)]) {
    expect(isValidSessionName(bad)).toBe(false);
  }
  for (const ok of ["a", "ok-name_1", "a".repeat(128), "-x", "UPPER"]) {
    expect(isValidSessionName(ok)).toBe(true);
  }
});

test("a shell that cannot edit a line is never the session shell", () => {
  // Started from a nix devshell, $SHELL is the devshell's own bash, built without
  // readline: no `bind`, no `complete`, no line editing at all. A session created
  // with it echoes every arrow key as `^[[D`, cannot be walked back past a
  // wrapped line, and prints the prompt's \[ \] markers, so the prompt itself
  // looks broken. The host's answer in /etc/passwd is what counts.
  const passwd = "ldeen:x:1000:1000::/home/ldeen:/bin/sh\n";
  expect(resolveLoginShell(1000, { SHELL: "/bin/sh" } as NodeJS.ProcessEnv, passwd)).toBe("/bin/bash");
  // A zsh login shell is honoured: it has an editor of its own.
  const zsh = existsSync("/usr/bin/zsh") || existsSync("/bin/zsh");
  if (zsh) {
    const p = "ldeen:x:1000:1000::/home/ldeen:/usr/bin/zsh\n";
    const got = resolveLoginShell(1000, { SHELL: "/bin/sh" } as NodeJS.ProcessEnv, p);
    expect(got === "/usr/bin/zsh" || got === "/bin/zsh").toBe(true);
  }
});

test("resolveLoginShell prefers the user's login shell over the inherited $SHELL", () => {
  const passwd = "root:x:0:0:root:/root:/bin/sh\nldeen:x:1000:1000::/home/ldeen:/bin/bash\n";
  // The passwd login shell wins: a daemon started from a devshell must not hand
  // sessions that devshell's $SHELL (the bug that spammed every new shell).
  expect(resolveLoginShell(1000, { SHELL: "/nonexistent/zzz" } as NodeJS.ProcessEnv, passwd)).toBe("/bin/bash");
  // Unknown uid, and the inherited $SHELL is `sh`: bash is used instead, because
  // `sh` has no line editor.
  expect(resolveLoginShell(4242, { SHELL: "/bin/sh" } as NodeJS.ProcessEnv, passwd)).toBe("/bin/bash");
  // A passwd shell that does not exist is skipped, and so is the `sh` inherited
  // in its place.
  expect(resolveLoginShell(5, { SHELL: "/bin/sh" } as NodeJS.ProcessEnv, "u:x:5:5::/:/no/such/shell\n")).toBe("/bin/bash");
  // An inherited bash is still honoured when there is no passwd entry at all.
  const bash = existsSync("/bin/bash") ? "/bin/bash" : "/usr/bin/bash";
  expect(resolveLoginShell(4242, { SHELL: bash } as NodeJS.ProcessEnv, "")).toBe(bash);
});

test("resizeWindow asks tmux for an exact size, clamped to sane bounds", async () => {
  // The stub records its argv instead of talking to tmux, so the assertion is
  // about the *command we construct*, not about tmux's own behaviour.
  const log = join(dir, "argv.log");
  const script = join(dir, "tmux");
  writeFileSync(script, `#!/bin/sh\nprintf '%s ' "$@" >> ${log}\necho >> ${log}\n`);
  chmodSync(script, 0o755);
  rmSync(log, { force: true });
  process.env.PATH = `${dir}:${realPath}`;

  const cp = new TmuxControlPlane("test-socket", 2000);
  await cp.resizeWindow("sess", 100, 30);
  await cp.resizeWindow("sess", 1, 1);
  await cp.resizeWindow("sess", 9999, 9999);

  const lines = readFileSync(log, "utf8").trim().split("\n");
  expect(lines.length).toBe(3);
  expect(lines[0]).toContain("resize-window -t sess -x 100 -y 30");
  // The floor keeps a pane usable (a client bugging out at 1x1 leaves a shell
  // nobody can recover); the ceiling stops a runaway request from demanding a
  // window whose capture would blow the stream's byte budget.
  expect(lines[1]).toContain("-x 20 -y 5");
  expect(lines[2]).toContain("-x 500 -y 300");
});

test("sendMouseClick writes a real SGR click straight to the pty", async () => {
  // The point of `-H`: these bytes are the *program's* protocol, not tmux's
  // vocabulary. They must reach the pty verbatim.
  const log = join(dir, "argv.log");
  const script = join(dir, "tmux");
  writeFileSync(script, `#!/bin/sh\nprintf '%s ' "$@" >> ${log}\necho >> ${log}\n`);
  chmodSync(script, 0o755);
  rmSync(log, { force: true });
  process.env.PATH = `${dir}:${realPath}`;

  const cp = new TmuxControlPlane("test-socket", 2000);
  // Cell (4,2) is 1-based (5,3) on the wire.
  await cp.sendMouseClick("sess", 4, 2);

  const line = readFileSync(log, "utf8").trim();
  expect(line).toContain("send-keys -t sess -H");
  // ESC [ < 0 ; 5 ; 3 M   then   ESC [ < 0 ; 5 ; 3 m
  const hex = line.slice(line.indexOf("-H ") + 3).trim().split(" ");
  expect(hex.join(" ")).toBe(
    "1b 5b 3c 30 3b 35 3b 33 4d 1b 5b 3c 30 3b 35 3b 33 6d",
  );
});

test("sendBytes refuses to send nothing", async () => {
  const log = join(dir, "argv2.log");
  const script = join(dir, "tmux");
  writeFileSync(script, `#!/bin/sh\nprintf '%s ' "$@" >> ${log}\necho >> ${log}\n`);
  chmodSync(script, 0o755);
  rmSync(log, { force: true });
  process.env.PATH = `${dir}:${realPath}`;
  const cp = new TmuxControlPlane("test-socket", 2000);
  await cp.sendBytes("sess", new Uint8Array(0));
  expect(existsSync(log)).toBe(false);
});

test("split-right halves the columns and split-down the rows", () => {
  // tmux's `-h` is the *side by side* split, which reads backwards from the name
  // and is invisible when wrong: both succeed and only the shape differs.
  // Measured on 3.7b — plain `split-window` took a 120x40 pane to 120x19, and
  // `split-window -h` took it to 59x40.
  expect(tmuxActionArgs("split-right", "s")).toEqual(["split-window", "-h", "-t", "s"]);
  expect(tmuxActionArgs("split-down", "s")).toEqual(["split-window", "-t", "s"]);
  expect(tmuxActionArgs("new-window", "s")).toEqual(["new-window", "-t", "s"]);
});

test("tmuxActionArgs always targets the named session", () => {
  // Every action goes to the session the client is on, never to whatever tmux
  // considers current — the daemon is shared, and "current" belongs to nobody.
  for (const action of ["split-right", "split-down", "new-window"] as const) {
    const args = tmuxActionArgs(action, "ldmux-abc");
    const at = args.indexOf("-t");
    // `-t` immediately before the name, and the name last, whatever flags precede.
    expect(args[at + 1]).toBe("ldmux-abc");
    expect(args[args.length - 1]).toBe("ldmux-abc");
  }
});
