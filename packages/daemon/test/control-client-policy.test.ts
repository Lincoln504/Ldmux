import { test, expect } from "../../testing/src/expect.ts";
import { ContinueCoalescer, matchWaiter } from "../src/tmux/control-client.ts";
import { setTimeout as sleep } from "node:timers/promises";


test("an exact sequence match wins even when it is not the oldest waiter", () => {
  expect(matchWaiter([1, 2, 3], 3, true)).toBe(2);
  expect(matchWaiter([1, 2, 3], 1, true)).toBe(0);
});

test("offset numbering falls back to the oldest waiter (self-healing)", () => {
  // tmux's counter is one ahead of ours: the block cannot match by sequence,
  // and it must still resolve the pending caller rather than be dropped.
  expect(matchWaiter([1], 2, true)).toBe(0);
  expect(matchWaiter([5, 6], 7, true)).toBe(0);
});

test("an unnumbered block falls back to the oldest waiter after attach", () => {
  expect(matchWaiter([1, 2], -1, true)).toBe(0);
});

test("before attach an unmatched block is ignored (tmux's unsolicited one)", () => {
  expect(matchWaiter([], 0, false)).toBe(-1);
  expect(matchWaiter([1], 9, false)).toBe(-1);
  // …but a real sequence match still works even before %session-changed.
  expect(matchWaiter([1], 1, false)).toBe(0);
});

test("after attach a response always resolves someone if anyone is waiting", () => {
  // The anti-hang property, stated directly: no block is ever dropped while a
  // caller is pending (a hang is silent; a mis-association is loud).
  for (const seq of [-1, 0, 1, 2, 3, 99]) {
    expect(matchWaiter([1, 2], seq, true)).toBeGreaterThanOrEqual(0);
  }
  // With nobody waiting there is nothing to resolve.
  expect(matchWaiter([], 7, true)).toBe(-1);
});

test("pause resumes coalesce onto one trailing send per window", async () => {
  const sent: string[] = [];
  const coalescer = new ContinueCoalescer((pane) => sent.push(pane), 30);
  coalescer.request("%0");
  coalescer.request("%0"); // still inside the window: coalesced, never dropped
  coalescer.request("%0");
  expect(coalescer.pendingCount).toBe(1);
  expect(sent).toEqual([]);
  await sleep(80);
  expect(sent).toEqual(["%0"]); // exactly one resume, not zero and not three

  // A later pause starts a fresh window (tmux emits one %pause per transition,
  // so every transition must eventually produce a resume).
  coalescer.request("%0");
  await sleep(80);
  expect(sent).toEqual(["%0", "%0"]);

  // Panes are independent.
  coalescer.request("%1");
  coalescer.request("%2");
  await sleep(80);
  expect(sent).toEqual(["%0", "%0", "%1", "%2"]);
});

test("close cancels a pending resume", async () => {
  const sent: string[] = [];
  const coalescer = new ContinueCoalescer((pane) => sent.push(pane), 30);
  coalescer.request("%0");
  coalescer.clear();
  await sleep(60);
  expect(sent).toEqual([]);
  expect(coalescer.pendingCount).toBe(0);
});