import { test, expect } from "bun:test";
import { mirrorArgv, mirrorCommand, mirrorTmuxArgs } from "../src/desktop.ts";

test("mirror is read-only by default (input stays with the phone)", () => {
  expect(mirrorTmuxArgs("sock", "ldmux-x")).toEqual(["-L", "sock", "attach", "-r", "-t", "ldmux-x"]);
});

test("mirror can be made writable with readOnly:false", () => {
  expect(mirrorTmuxArgs("sock", "ldmux-x", { readOnly: false })).toEqual(["-L", "sock", "attach", "-t", "ldmux-x"]);
});

test("mirrorArgv wraps tmux in the terminal with an i3-targetable class", () => {
  expect(mirrorArgv("sock", "ldmux-x")).toEqual([
    "alacritty",
    "--class",
    "ldmux-mirror",
    "-e",
    "tmux",
    "-L",
    "sock",
    "attach",
    "-r",
    "-t",
    "ldmux-x",
  ]);
});

test("mirrorCommand renders a copy-pasteable command line", () => {
  expect(mirrorCommand("sock", "ldmux-x")).toBe(
    "alacritty --class ldmux-mirror -e tmux -L sock attach -r -t ldmux-x",
  );
  expect(mirrorCommand("sock", "ldmux-x", { terminal: "kitty", windowClass: "my-class" })).toContain("kitty --class my-class");
});
test("mirrorCommand shell-quotes every argument, so i3-msg exec cannot inject", () => {
  // i3-msg exec runs its argument through a shell, and it only takes one
  // space-free metacharacter to escape. Each of these must be quoted.
  const cmd = mirrorCommand("sock", "x$(id)", { terminal: "kitty; rm -rf /", windowClass: "c`whoami`" });
  expect(cmd).toContain("'x$(id)'");
  expect(cmd).toContain("'kitty; rm -rf /'");
  expect(cmd).toContain("'c`whoami`'");
  // A single quote inside a single-quoted string is escaped, not left dangling.
  expect(mirrorCommand("sock", "a'b")).toContain(`'a'\\''b'`);
  // Ordinary args stay bare for readability.
  expect(mirrorCommand("sock", "ldmux-x")).toBe("alacritty --class ldmux-mirror -e tmux -L sock attach -r -t ldmux-x");
});
