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