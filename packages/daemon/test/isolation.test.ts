import { test, expect } from "../../testing/src/expect.ts";
import { childEnv } from "../src/tmux/control-plane.ts";
import { resolveTmuxSocket } from "../../protocol/src/paths.ts";

/**
 * ldmux drives its own tmux server, and these are the two ways that could stop
 * being true. Both are about somebody else's tmux: the whole reason the global
 * option profile (status off, mouse on, history-limit, default-shell, COLORTERM)
 * is safe is that it lands on `ldmux` and not on `default`.
 */

test("a tmux child does not inherit the user's TMUX", () => {
  // The daemon passes its own environment to every tmux invocation, so a daemon
  // started from inside the user's tmux would hand *their* `TMUX` to every shell
  // it creates. `TMUX` names the server a client is attached to, so a program
  // inside an ldmux shell would believe it was in the user's session and act on
  // it. An ldmux shell is attached to nothing.
  const env = childEnv({
    PATH: "/usr/bin",
    TMUX: "/tmp/tmux-1000/default,12345,0",
    TMUX_PANE: "%7",
    HOME: "/home/someone",
  });
  expect(env.TMUX).toBe(undefined);
  expect(env.TMUX_PANE).toBe(undefined);
  // ...and nothing else is dropped: this is a subtraction of two variables, not
  // a fresh environment.
  expect(env.PATH).toBe("/usr/bin");
  expect(env.HOME).toBe("/home/someone");
});

test("the tmux socket is never the user's", () => {
  const before = process.env.LDMUX_TMUX_SOCK;
  try {
    delete process.env.LDMUX_TMUX_SOCK;
    // The default is a server of ldmux's own, not `default`.
    expect(resolveTmuxSocket()).toBe("ldmux");

    process.env.LDMUX_TMUX_SOCK = "ldmux-staging";
    expect(resolveTmuxSocket()).toBe("ldmux-staging");

    // Pointing it at the user's server is refused rather than obeyed. It is the
    // plausible mistake (wanting ldmux to "see your existing sessions") and the
    // damage is silent: `status off` alone takes away their status bar.
    process.env.LDMUX_TMUX_SOCK = "default";
    let threw = false;
    try {
      resolveTmuxSocket();
    } catch {
      threw = true;
    }
    expect(threw).toBe(true);
  } finally {
    if (before === undefined) delete process.env.LDMUX_TMUX_SOCK;
    else process.env.LDMUX_TMUX_SOCK = before;
  }
});
