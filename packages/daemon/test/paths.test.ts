import { test, expect, afterEach } from "../../testing/src/expect.ts";
import { resolveSocketPath, resolveStateDir, resolveTmuxSocket } from "../../protocol/src/paths.ts";

const KEYS = ["LDMUX_STATE_DIR", "LDMUX_SOCK", "LDMUX_TMUX_SOCK", "XDG_RUNTIME_DIR"] as const;
const saved = Object.fromEntries(KEYS.map((k) => [k, process.env[k]]));

afterEach(() => {
  for (const key of KEYS) {
    if (saved[key] === undefined) delete process.env[key];
    else process.env[key] = saved[key];
  }
});

function clearAll(): void {
  for (const key of KEYS) delete process.env[key];
}

test("the state dir prefers LDMUX_STATE_DIR, then XDG_RUNTIME_DIR, then /tmp", () => {
  clearAll();
  expect(resolveStateDir()).toBe("/tmp/ldmux");

  process.env.XDG_RUNTIME_DIR = "/run/user/1000";
  expect(resolveStateDir()).toBe("/run/user/1000/ldmux");

  process.env.LDMUX_STATE_DIR = "/var/tmp/ldmux-test";
  expect(resolveStateDir()).toBe("/var/tmp/ldmux-test");
  // An explicit state dir wins regardless of what else is set.
  process.env.XDG_RUNTIME_DIR = "/ignored";
  expect(resolveStateDir()).toBe("/var/tmp/ldmux-test");
});

test("the socket path follows the state dir unless overridden", () => {
  clearAll();
  process.env.LDMUX_STATE_DIR = "/s";
  expect(resolveSocketPath()).toBe("/s/ldmuxd.sock");

  process.env.LDMUX_SOCK = "/other.sock";
  expect(resolveSocketPath()).toBe("/other.sock");
});

test("the tmux socket defaults to the private server name", () => {
  clearAll();
  expect(resolveTmuxSocket()).toBe("ldmux");
  process.env.LDMUX_TMUX_SOCK = "ldmux-test";
  expect(resolveTmuxSocket()).toBe("ldmux-test");
});

test("all three resolvers agree on one state dir (the contract that cannot drift)", () => {
  clearAll();
  process.env.LDMUX_STATE_DIR = "/tmp/one-place";
  expect(resolveSocketPath().startsWith(resolveStateDir())).toBe(true);
});
