import { test, expect, afterAll } from "bun:test";
import { TmuxControlClient } from "../src/tmux/control-client.ts";

const SOCK = `ldmux-test-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const tm = (...args: string[]) => Bun.spawnSync(["tmux", "-L", SOCK, ...args]);

afterAll(() => {
  tm("kill-server");
});

async function waitFor(predicate: () => boolean, ms = 4000): Promise<void> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline && !predicate()) await Bun.sleep(40);
}

test("live: control mode lists, runs commands, and streams output over pipes", async () => {
  expect(tm("new-session", "-d", "-s", "s1", "-x", "100", "-y", "30", "bash", "--norc", "--noprofile").exitCode).toBe(0);
  expect(tm("has-session", "-t", "s1").exitCode).toBe(0);

  const output: string[] = [];
  let stderr = "";
  const client = await TmuxControlClient.connect(SOCK, "s1", {
    onOutput: (_pane, text) => output.push(text),
    onStderr: (s) => (stderr += s),
  });

  // Command/response correlation.
  expect(await client.command("display-message -p '#{session_name}'")).toEqual(["s1"]);

  // Literal text + a named key.
  await client.sendText("s1", "printf 'hello-ldmux\\n'");
  await client.sendKey("s1", "Enter");
  await waitFor(() => output.join("").includes("hello-ldmux"));
  expect(output.join("")).toContain("hello-ldmux");

  // UTF-8 survives octal decoding + per-pane reassembly. Emit the bytes from
  // the shell (printf \xHH) so no multibyte input crosses send-keys.
  output.length = 0;
  await client.sendText("s1", "printf '\\xf0\\x9f\\x98\\x80\\n'");
  await client.sendKey("s1", "Enter");
  await waitFor(() => output.join("").includes("😀"));
  expect(output.join("")).toContain("😀");

  expect(stderr).toBe("");
  client.close();
});

test("live: control mode errors are reported", async () => {
  expect(tm("new-session", "-d", "-s", "s2", "bash", "--norc", "--noprofile").exitCode).toBe(0);
  const client = await TmuxControlClient.connect(SOCK, "s2");
  await expect(client.command("this-is-not-a-tmux-command")).rejects.toThrow();
  client.close();
});

test("live: geometry is pinned with window-size manual (no per-viewer reflow)", async () => {
  expect(tm("new-session", "-d", "-s", "s3", "-x", "100", "-y", "30", "bash", "--norc", "--noprofile").exitCode).toBe(0);
  tm("set-option", "-g", "window-size", "manual");
  const client = await TmuxControlClient.connect(SOCK, "s3");
  expect(await client.command("display-message -p -t s3:0 '#{window_width}x#{window_height}'")).toEqual(["100x30"]);
  client.close();
});