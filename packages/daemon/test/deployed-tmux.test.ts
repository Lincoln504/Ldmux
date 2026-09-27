import { test, expect, afterAll } from "../../testing/src/expect.ts";
import { existsSync, mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { TmuxControlPlane, resolveTmuxBin } from "../src/tmux/control-plane.ts";
import { killTmuxServer } from "./tmux-cleanup.ts";

/**
 * The daemon's server profile, against every tmux this machine could run it with.
 *
 * The suite runs on the devShell's tmux. The deployed daemon runs on the flake's
 * runtime tmux (`LDMUX_TMUX_BIN`, set by the unit to `%S/ldmux/runtime/bin/tmux`),
 * which is the same 3.7b — and this docstring said the opposite until 2026-09-27.
 *
 * The candidates below are what a daemon started by hand, or on a host with no
 * installed runtime, would resolve from PATH — the system's tmux. On 2026-09-26 that
 * difference was total: the profile's global `window-size manual` crashes tmux 3.5a
 * on the first `new-session`, so a fallback daemon could not create a single shell
 * while every test passed. This test exists so that every binary that could be
 * resolved is exercised, whether or not it is the one the unit names.
 */
const candidates = [...new Set([resolveTmuxBin(), "/usr/bin/tmux", "/usr/local/bin/tmux"])].filter((b) => existsSync(b));
const dir = mkdtempSync(join(tmpdir(), "ldmux-deployed-"));
const sockets: { socket: string; bin: string }[] = [];

afterAll(() => {
  for (const s of sockets) killTmuxServer(s.socket, s.bin);
  rmSync(dir, { recursive: true, force: true });
});

for (const bin of candidates) {
  test(`a cold server with the daemon's profile can create a shell (${bin})`, async () => {
    const socket = `ldmux-deployed-${process.pid}-${sockets.length}`;
    sockets.push({ socket, bin });
    const cp = new TmuxControlPlane(socket, undefined, bin, join(dir, `${socket}.conf`));
    await cp.ensureServer();
    await cp.newSession({ name: "s1", cwd: tmpdir(), cols: 80, rows: 24 });
    await cp.newSession({ name: "s2", cwd: tmpdir(), cols: 80, rows: 24 });
    const names = (await cp.listSessions())?.map((s) => s.name);
    expect(names).toEqual(["s1", "s2"]);
    // ...and the size is still the daemon's to set.
    await cp.resizeWindow("s1", 60, 20);
    expect(await cp.displayMessage("s1", "#{window_width}x#{window_height}")).toBe("60x20");
    // ...and every option in the profile reads back as written. A line tmux could
    // not parse is reported by putting the first attached pane into view-mode,
    // which swallows every key sent to it — so a profile that "mostly loaded" is a
    // shell the phone cannot type into. Every `set-option -g NAME VALUE` is
    // checked, not a sample: the one that broke was a sample nobody chose.
    const { spawnSync } = await import("node:child_process");
    for (const argv of cp.serverOptionArgs()) {
      const [cmd, flags, name, value] = argv;
      if (cmd !== "set-option" || !["-g", "-sg"].includes(flags!) || value === undefined) continue;
      const shown = spawnSync(bin, ["-L", socket, "show-options", flags === "-sg" ? "-sv" : "-gv", name!], {
        encoding: "utf8",
      }).stdout.trim();
      expect(shown, `${name} after loading the profile`).toBe(value);
    }
  });
}

for (const bin of candidates) {
  test(`a server that is already running takes the current profile (${bin})`, async () => {
    const socket = `ldmux-deployed-run-${process.pid}-${sockets.length}`;
    sockets.push({ socket, bin });
    // A server started before the profile changed: no profile, old options.
    const bare = new TmuxControlPlane(socket, undefined, bin);
    await bare.newSession({ name: "old", cwd: tmpdir(), cols: 80, rows: 24 });
    const { spawnSync } = await import("node:child_process");
    spawnSync(bin, ["-L", socket, "set-option", "-g", "remain-on-exit", "on"]);
    const cp = new TmuxControlPlane(socket, undefined, bin, join(dir, `${socket}.conf`));
    await cp.ensureServer();
    expect(await cp.displayMessage("old", "#{remain-on-exit}")).toBe("off");
    expect(await cp.displayMessage("old", "#{set-titles}")).toBe("1");
  });
}
