import { test, expect, afterAll } from "bun:test";
import { createServer, type Server } from "node:net";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { call } from "../src/socket-client.ts";

/**
 * The socket client is the CLI's and the extension's only way to talk to the
 * daemon, so its failure modes matter as much as its happy path.
 */

const dir = mkdtempSync(join(tmpdir(), "ldmux-sockclient-"));
const servers: Server[] = [];

/** A Unix-socket server that answers with `respond` (or never, if omitted). */
async function serve(name: string, respond?: (sock: import("node:net").Socket) => void): Promise<string> {
  const path = join(dir, `${name}.sock`);
  const server = createServer((sock) => {
    sock.on("data", () => {
      if (respond) respond(sock);
    });
  });
  servers.push(server);
  await new Promise<void>((resolve) => server.listen(path, resolve));
  return path;
}

afterAll(() => {
  for (const s of servers) s.close();
  rmSync(dir, { recursive: true, force: true });
});

test("a valid response resolves with the result", async () => {
  const path = await serve("ok", (s) => s.write(JSON.stringify({ id: 1, ok: true, result: { pong: true } }) + "\n"));
  expect(await call<{ pong: boolean }>(path, "ping")).toEqual({ pong: true });
});

test("an ok:false response rejects with the server's error", async () => {
  const path = await serve("err", (s) => s.write(JSON.stringify({ id: 1, ok: false, error: "no such session" }) + "\n"));
  await expect(call(path, "capture")).rejects.toThrow(/no such session/);
});

test("malformed JSON rejects instead of hanging", async () => {
  const path = await serve("bad", (s) => s.write("not json at all\n"));
  await expect(call(path, "ping")).rejects.toThrow();
});

test("a server that never answers hits the timeout", async () => {
  const path = await serve("silent"); // accepts, never writes
  const started = Date.now();
  await expect(call(path, "ping", {}, 200)).rejects.toThrow(/timed out/);
  expect(Date.now() - started).toBeGreaterThanOrEqual(200);
});

test("a closed socket rejects cleanly", async () => {
  const path = await serve("close", (s) => {
    s.write(JSON.stringify({ id: 1, ok: true, result: 1 }).slice(0, 5)); // partial, no newline
    s.end(); // then close, without ever completing the line
  });
  await expect(call(path, "ping")).rejects.toThrow(/closed|timed out/);
});

test("a missing socket rejects (connection refused)", async () => {
  await expect(call(join(dir, "nope.sock"), "ping", {}, 500)).rejects.toThrow();
});

test("a multi-byte character split across reads is reassembled", async () => {
  const body = JSON.stringify({ id: 1, ok: true, result: { title: "café-Ω" } }) + "\n";
  const bytes = Buffer.from(body, "utf8");
  const cut = bytes.indexOf(Buffer.from("é")) + 1; // split inside é
  const path = await serve("mb", (s) => {
    s.write(bytes.subarray(0, cut));
    setTimeout(() => s.write(bytes.subarray(cut)), 10);
  });
  expect((await call<{ title: string }>(path, "ping")).title).toBe("café-Ω");
});
