import { test, expect } from "../../testing/src/expect.ts";
import { CLEAR_AND_HOME, screenUpdate } from "../src/tmux/screen-diff.ts";

test("identical screens produce no output", () => {
  expect(screenUpdate("abc", "abc")).toEqual({ kind: "none", text: "" });
});

test("first capture is a plain append", () => {
  expect(screenUpdate("", "hello")).toEqual({ kind: "append", text: "hello" });
});

test("appended output is forwarded verbatim", () => {
  expect(screenUpdate("$ ls\n", "$ ls\nfile\n")).toEqual({ kind: "append", text: "file\n" });
});

test("a rewritten row is addressed in place, never repainted", () => {
  // The interactive-shell case: the prompt line changes, the rest does not.
  const prev = "line one\n$ \nline three\n";
  const next = "line one\n$ ls -a\nline three\n";
  const update = screenUpdate(prev, next);
  expect(update.kind).toBe("rows");
  expect(update.text).toBe("\x1b[2;1H\x1b[K$ ls -a");
});

test("only the changed middle rows are rewritten", () => {
  const prev = "a\nb\nc\nd\ne\n";
  const next = "a\nB\nc\nd\ne\n";
  expect(screenUpdate(prev, next).text).toBe("\x1b[2;1H\x1b[KB");
});

test("a different row count repaints instead of guessing", () => {
  const update = screenUpdate("x\ny\n", "a\nb\nc\n");
  expect(update.kind).toBe("repaint");
  expect(update.text).toBe(CLEAR_AND_HOME + "a\nb\nc\n");
});

test("a scrolled screen (equal rows, shifted content) is rewritten row-wise", () => {
  const prev = "1\n2\n3\n";
  const next = "2\n3\n4\n";
  const update = screenUpdate(prev, next);
  expect(update.kind).toBe("rows");
  // Nothing in common with the same index, so every row is rewritten.
  expect(update.text).toBe("\x1b[1;1H\x1b[K2\x1b[2;1H\x1b[K3\x1b[3;1H\x1b[K4");
});

test("multi-byte characters are compared whole, never split", () => {
  const update = screenUpdate("héllo 😀", "héllo 😀!");
  expect(update).toEqual({ kind: "append", text: "!" });
});

test("an escape sequence appended mid-screen is preserved", () => {
  const update = screenUpdate("\x1b[31mred", "\x1b[31mred\x1b[0m");
  expect(update).toEqual({ kind: "append", text: "\x1b[0m" });
});

test("empty previous row set cannot cause an empty-rows update", () => {
  expect(screenUpdate("", "").kind).toBe("none");
});