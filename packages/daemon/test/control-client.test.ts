import { test, expect, afterAll } from "bun:test";
import { TmuxControlClient } from "../src/tmux/control-client.ts";
import { killTmuxServer } from "./tmux-cleanup.ts";

const SOCK = `ldmux-test-${process.pid}-${Math.random().toString(36).slice(2, 8)}`;
const tm = (...args: string[]) => Bun.spawnSync(["tmux", "-L", SOCK, ...args]);

afterAll(() => {
  killTmuxServer(SOCK);
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
test("live: concurrent commands are matched to their own responses by sequence", async () => {
  tm("new-session", "-d", "-s", "s2", "-x", "100", "-y", "30", "bash", "--norc", "--noprofile");
  const client = await TmuxControlClient.connect(SOCK, "s2", {});
  // Issued without awaiting in between: with a naive FIFO both would still work
  // in order, but a sequence mismatch (tmux numbering) would desynchronize the
  // stream, so assert each promise sees its own command's output.
  const [a, b, c] = await Promise.all([
    client.command("display-message -p 'AAA'"),
    client.command("display-message -p 'BBB'"),
    client.command(["display-message", "-p", "CCC"]),
  ]);
  expect(a).toEqual(["AAA"]);
  expect(b).toEqual(["BBB"]);
  expect(c).toEqual(["CCC"]);
  // The stream is still correlated afterwards.
  expect(await client.command("display-message -p '#{session_name}'")).toEqual(["s2"]);
  client.close();
  tm("kill-session", "-t", "s2");
});

test("live: the resume command used on %pause is accepted by tmux", async () => {
  // If this syntax were wrong, a paused pane would never resume and the
  // control stream would stall silently (the failure is swallowed by .catch).
  tm("new-session", "-d", "-s", "s3", "-x", "100", "-y", "30", "bash", "--norc", "--noprofile");
  const client = await TmuxControlClient.connect(SOCK, "s3", {});
  const pane = (await client.command("display-message -p '#{pane_id}'"))[0];
  expect(pane.startsWith("%")).toBe(true);
  await expect(client.command(["refresh-client", "-A", `${pane}:continue`])).resolves.toBeDefined();
  client.close();
  tm("kill-session", "-t", "s3");
});
