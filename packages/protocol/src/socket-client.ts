/**
 * Newline-delimited JSON client for the daemon's Unix socket.
 *
 * Uses node:net: the same client serves the CLI, the tests, and the
 * mobile/web clients.
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
    // Incremental decode: a multi-byte character split across TCP chunks would
    // otherwise be mangled into U+FFFD in the response body.
    const decoder = new TextDecoder();
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
      buf += decoder.decode(chunk, { stream: true });
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