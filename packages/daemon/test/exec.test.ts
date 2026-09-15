import { test, expect } from "../../testing/src/expect.ts";
import { run, EXEC_TIMEOUT_MS } from "../src/exec.ts";

test("captures stdout, stderr and a zero exit code", async () => {
  const res = await run(["sh", "-c", "echo out; echo err >&2"]);
  expect(res.code).toBe(0);
  expect(res.out.trim()).toBe("out");
  expect(res.err.trim()).toBe("err");
});

test("a non-zero exit is reported as its own code, not the -1 sentinel", async () => {
  const res = await run(["sh", "-c", "exit 3"]);
  expect(res.code).toBe(3);
});

test("a missing binary is reported, never thrown", async () => {
  const res = await run(["definitely-not-a-real-binary-ldmux"]);
  expect(res.code).toBe(-1);
  expect(res.err).toBeTruthy();
});

test("both pipes are drained concurrently, so a full pipe cannot deadlock", async () => {
  // 1 MiB on each stream is far beyond the ~64 KiB kernel pipe buffer: if either
  // stream were read only after the other finished, this would hang.
  const res = await run([
    "sh",
    "-c",
    "head -c 1048576 /dev/zero | tr '\\0' a; head -c 1048576 /dev/zero | tr '\\0' b >&2",
  ]);
  expect(res.code).toBe(0);
  expect(res.out.length).toBe(1048576);
  expect(res.err.length).toBe(1048576);
});

test("a wedged command is killed at the timeout and reports it", async () => {
  const started = Date.now();
  const res = await run(["sh", "-c", "sleep 30"], { timeoutMs: 250 });
  const elapsed = Date.now() - started;
  expect(res.code).toBe(-1);
  expect(res.err).toContain("timed out");
  // The kill really happened rather than the command finishing on its own.
  expect(elapsed).toBeGreaterThanOrEqual(250);
  expect(elapsed).toBeLessThan(5000);
});

test("an explicit env replaces the child environment", async () => {
  const res = await run(["sh", "-c", "printf %s \"$LDMUX_EXEC_TEST\""], {
    env: { ...process.env, LDMUX_EXEC_TEST: "injected" },
  });
  expect(res.out).toBe("injected");
});

test("the default timeout is a finite, sane bound", () => {
  expect(EXEC_TIMEOUT_MS).toBeGreaterThan(0);
  expect(EXEC_TIMEOUT_MS).toBeLessThanOrEqual(60_000);
});