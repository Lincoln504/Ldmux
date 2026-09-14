/**
 * Newline-delimited JSON client for the daemon's Unix socket.
 *
 * Uses node:net (not Bun.connect) so the same client works under Bun (daemon
 * tests/CLI in the devShell) and under Node (the pi extension, which runs in
 * the pi process). Used by the CLI, tests, and the pi extension.
 *
 * One call == one connection == one request/response; the first LF-terminated
 * line is the answer. (Do not multiplex requests on one connection without
 * adding id-correlation first.)
 */

import { connect } from "node:net";

export interface RpcResponse<T> {
  id: number | null;
  ok: boolean;
  result?: T;
  error?: string;
}

export function call<T = unknown>(
  socketPath: string,
  method: string,
  params: Record<string, unknown> = {},
  timeoutMs = 8000,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const sock = connect(socketPath);
    let buf = "";
    let done = false;
    const finish = (fn: () => void) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      sock.destroy();
      fn();
    };
    const timer = setTimeout(
      () => finish(() => reject(new Error(`ldmux request timed out: ${method}`))),
      timeoutMs,
    );
    sock.on("connect", () => {
      sock.write(JSON.stringify({ id: 1, method, params }) + "\n");
    });
    sock.on("data", (chunk: Buffer) => {
      buf += chunk.toString("utf8");
      const nl = buf.indexOf("\n");
      if (nl < 0) return;
      const line = buf.slice(0, nl);
      finish(() => {
        try {
          const res = JSON.parse(line) as RpcResponse<T>;
          if (res.ok) resolve(res.result as T);
          else reject(new Error(res.error || "ldmux request failed"));
        } catch (err) {
          reject(err as Error);
        }
      });
    });
    sock.on("error", (err) => finish(() => reject(err)));
    sock.on("close", () => finish(() => reject(new Error("ldmux socket closed"))));
  });
}