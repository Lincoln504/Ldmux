import assert from "node:assert/strict";
import test from "node:test";
import { SAY_MAX_CHARS, assistantText, cleanForSpeech, extractSay, stripSay } from "../src/say.ts";

const answer = "I updated `a.rs` and `b.rs`.\n\n```rust\nfn main() {}\n```\n\n```say\nDone. Two files changed.\n```\n";

test("extracts the spoken block and leaves other fences alone", () => {
  assert.equal(extractSay(answer), "Done. Two files changed.");
});

test("the last say block wins and an unfenced answer says nothing", () => {
  assert.equal(extractSay("```say\nfirst\n```\ntext\n```say\nsecond\n```"), "second");
  assert.equal(extractSay("no fence here"), null);
  assert.equal(extractSay("```say\n\n```"), null);
  assert.equal(extractSay("```say\nnever closed"), null, "an unterminated block is not complete");
});

test("the fence is hidden from the transcript, complete or still streaming", () => {
  assert.equal(stripSay(answer).includes("say"), false);
  assert.equal(stripSay(answer).includes("fn main"), true, "other code blocks stay");
  assert.equal(stripSay("hello\n```say\nstill typ"), "hello");
});

test("spoken text is cleaned and capped", () => {
  assert.equal(cleanForSpeech("a\u0000b\u0007  c\n\nd"), "ab c d");
  const long = "Sentence number one is here. ".repeat(100);
  const c = cleanForSpeech(long);
  assert.ok(c.length <= SAY_MAX_CHARS);
  assert.ok(c.endsWith("."), "cut at a sentence end");
});

test("only assistant text blocks are read", () => {
  const msg = { role: "assistant", content: [{ type: "thinking", thinking: "```say\nsecret\n```" }, { type: "text", text: "hi" }, { type: "toolCall", name: "x" }] };
  assert.equal(assistantText(msg), "hi");
  assert.equal(assistantText({ role: "user", content: [{ type: "text", text: "```say\nx\n```" }] }), "");
});
