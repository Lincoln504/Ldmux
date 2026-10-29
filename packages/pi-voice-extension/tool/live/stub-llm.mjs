// OpenAI-compatible SSE stub. Records each request body; the reply says whether the system
// prompt carried the voice section, and ends with a say fence only if it did.
import { createServer } from "node:http";
import { appendFileSync } from "node:fs";
const port = Number(process.argv[2]);
createServer((req, res) => {
  let b = ""; req.on("data", c => b += c); req.on("end", () => {
    appendFileSync("/tmp/voice-live/llm-requests.jsonl", b + "\n");
    let j = {}; try { j = JSON.parse(b); } catch {}
    const sys = (j.messages || []).filter(m => m.role === "system" || m.role === "developer").map(m => typeof m.content === "string" ? m.content : JSON.stringify(m.content)).join("\n");
    const voice = sys.includes("VOICE MODE IS ON");
    const text = voice ? "Here is the detailed answer.\n\n```say\nAll done. The stub answered in voice mode.\n```" : "Here is the detailed answer, no voice section was present.";
    res.writeHead(200, { "content-type": "text/event-stream" });
    const chunk = (delta, fin) => `data: ${JSON.stringify({ id: "c1", object: "chat.completion.chunk", created: 1, model: "stub-1", choices: [{ index: 0, delta, finish_reason: fin }], ...(fin ? { usage: { prompt_tokens: 5, completion_tokens: 5, total_tokens: 10 } } : {}) })}\n\n`;
    res.write(chunk({ role: "assistant", content: "" }, null));
    for (const part of text.match(/.{1,12}/gs)) res.write(chunk({ content: part }, null));
    res.write(chunk({}, "stop")); res.write("data: [DONE]\n\n"); res.end();
  });
}).listen(port, "127.0.0.1");
