import { test, expect } from "bun:test";
import { decodeOctalEscapes } from "../src/tmux/octal.ts";
import { ControlParser } from "../src/tmux/control-parser.ts";

const enc = (s: string) => new TextEncoder().encode(s);
const dec = (b: Uint8Array) => new TextDecoder().decode(b);

test("octal: decodes control chars and backslash", () => {
  expect([...decodeOctalEscapes(enc("\\015\\012"))]).toEqual([13, 10]);
  expect([...decodeOctalEscapes(enc("\\134"))]).toEqual([92]); // literal backslash
  expect([...decodeOctalEscapes(enc("a\\033[0mb"))]).toEqual([97, 27, 91, 48, 109, 98]);
});

test("octal: leaves non-escapes intact", () => {
  expect(dec(decodeOctalEscapes(enc("hello")))).toBe("hello");
  expect(dec(decodeOctalEscapes(enc("\\9\\99")))).toBe("\\9\\99");
});

test("octal: passes raw UTF-8 bytes through uncorrupted", () => {
  const bytes = enc("😀");
  expect([...decodeOctalEscapes(bytes)]).toEqual([0xf0, 0x9f, 0x98, 0x80]);
  expect(dec(decodeOctalEscapes(bytes))).toBe("😀");
});

test("parser: correlates a command response block", () => {
  const responses: Array<[number, boolean, string[]]> = [];
  const parser = new ControlParser({ onResponse: (s, ok, l) => responses.push([s, ok, l]) });
  parser.feed(enc("%begin 1 7 0\ns1\n%end 1 7 0\n"));
  expect(responses).toEqual([[7, true, ["s1"]]]);
});

test("parser: reports %error blocks", () => {
  const responses: Array<[number, boolean, string[]]> = [];
  const parser = new ControlParser({ onResponse: (s, ok, l) => responses.push([s, ok, l]) });
  parser.feed(enc("%begin 1 2 0\nbad\n%error 1 2 0\n"));
  expect(responses).toEqual([[2, false, ["bad"]]]);
});

test("parser: decodes %output", () => {
  const out: Array<[string, string]> = [];
  const parser = new ControlParser({ onOutput: (p, t) => out.push([p, t]) });
  parser.feed(enc("%output %0 echo hi\\015\\012"));
  parser.feed(enc("\n"));
  expect(out).toEqual([["%0", "echo hi\r\n"]]);
});

test("parser: decodes %extended-output", () => {
  const out: Array<[string, string]> = [];
  const parser = new ControlParser({ onOutput: (p, t) => out.push([p, t]) });
  parser.feed(enc("%extended-output %3 12 : hello\\015\\012\n"));
  expect(out).toEqual([["%3", "hello\r\n"]]);
});

test("parser: reassembles UTF-8 split across %output records", () => {
  // "😀" = f0 9f 98 80, split into two notifications.
  const out: string[] = [];
  const parser = new ControlParser({ onOutput: (_p, t) => out.push(t) });
  parser.feed(enc("%output %0 \\360\\237"));
  parser.feed(enc("\n"));
  parser.feed(enc("%output %0 \\230\\200\n"));
  expect(out).toEqual(["😀"]);
});

test("parser: handles records split across feed() chunks", () => {
  const out: string[] = [];
  const parser = new ControlParser({ onOutput: (_p, t) => out.push(t) });
  parser.feed(enc("%out"));
  parser.feed(enc("put %0 ok\\015\\012\n"));
  expect(out).toEqual(["ok\r\n"]);
});

test("parser: forwards unknown notifications and %exit", () => {
  const notes: string[] = [];
  let exited = false;
  const parser = new ControlParser({ onNotification: (l) => notes.push(l), onExit: () => { exited = true; } });
  parser.feed(enc("%session-changed $0 s1\n%window-add @1\n%exit\n"));
  expect(notes).toEqual(["%session-changed $0 s1", "%window-add @1"]);
  expect(exited).toBe(true);
});
test("parser: reports %pause and %continue with their pane id", () => {
  const pauses: string[] = [];
  const continues: string[] = [];
  const p = new ControlParser({ onPause: (pane) => pauses.push(pane), onContinue: (pane) => continues.push(pane) });
  p.feed(enc("%pause %1\n%continue %1\n%output %1 hi\n%pause %2\n"));
  expect(pauses).toEqual(["%1", "%2"]);
  expect(continues).toEqual(["%1"]);
});

test("parser: an unsolicited response block carries its own sequence", () => {
  const seen: Array<[number, boolean]> = [];
  const p = new ControlParser({ onResponse: (seq, ok) => seen.push([seq, ok]) });
  p.feed(enc("%begin 1 0 0\n%end 1 0 0\n%begin 1 4 0\n%end 1 4 0\n%begin 1 5 0\n%error 1 5 0\n"));
  expect(seen).toEqual([
    [0, true],
    [4, true],
    [5, false],
  ]);
});

test("parser: a non-numeric response sequence is reported as -1, not a crash", () => {
  const seen: Array<[number, boolean]> = [];
  const p = new ControlParser({ onResponse: (seq, ok) => seen.push([seq, ok]) });
  p.feed(enc("%begin 1 x 0\n%end 1 x 0\n"));
  expect(seen).toEqual([[-1, true]]);
});

test("parser: a malformed %extended-output is dropped, not thrown or garbled", () => {
  const out: Array<[string, string]> = [];
  const p = new ControlParser({ onOutput: (pane, text) => out.push([pane, text]) });
  // No " : " separator: the record has no payload; it must not crash or leak.
  expect(() => p.feed(enc("%extended-output %1 200\n"))).not.toThrow();
  expect(out).toEqual([]);
  // The next well-formed record still parses.
  p.feed(enc("%output %1 ok\n"));
  expect(out).toEqual([["%1", "ok"]]);
});

test("parser: a line split across two feeds is reassembled", () => {
  const out: Array<[string, string]> = [];
  const p = new ControlParser({ onOutput: (pane, text) => out.push([pane, text]) });
  p.feed(enc("%output %1 hel"));
  expect(out).toEqual([]); // no LF yet: nothing emitted
  p.feed(enc("lo\n"));
  expect(out).toEqual([["%1", "hello"]]);
});

test("parser: an unterminated block emits nothing until %end arrives", () => {
  const seen: Array<[number, boolean, string[]]> = [];
  const p = new ControlParser({ onResponse: (seq, ok, lines) => seen.push([seq, ok, lines]) });
  p.feed(enc("%begin 1 3 0\npartial\n"));
  expect(seen).toEqual([]);
  p.feed(enc("rest\n%end 1 3 0\n"));
  expect(seen).toEqual([[3, true, ["partial", "rest"]]]);
});
