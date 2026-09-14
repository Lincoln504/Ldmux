/**
 * Newline-delimited JSON client for the daemon's Unix socket.
 * Used by the CLI and tests.
 */

export async function call<T = unknown>(
  socketPath: string,
  method: string,
  params: Record<string, unknown> = {},
  timeoutMs = 8000,
): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    let buf = "";
    let settled = false;
    let sock: any;
    const finish = (fn: () => void) => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      fn();
    };
    const timer = setTimeout(() => {
      finish(() => {
        try {
          sock?.close();
        } catch {
          /* ignore */
        }
        reject(new Error(`ldmux request timed out: ${method}`));
      });
    }, timeoutMs);

    Bun.connect({
      unix: socketPath,
      socket: {
        open(s: any) {
          sock = s;
          s.write(JSON.stringify({ id: 1, method, params }) + "\n");
        },
        data(s: any, data: Uint8Array) {
          buf += new TextDecoder().decode(data);
          const nl = buf.indexOf("\n");
          if (nl < 0) return;
          const line = buf.slice(0, nl);
          finish(() => {
            try {
              s.close();
            } catch {
              /* ignore */
            }
            try {
              const res = JSON.parse(line) as { ok: boolean; result?: T; error?: string };
              if (res.ok) resolve(res.result as T);
              else reject(new Error(res.error || "ldmux request failed"));
            } catch (err) {
              reject(err as Error);
            }
          });
        },
        close() {
          finish(() => reject(new Error("ldmux socket closed")));
        },
        error(_s: any, err: any) {
          finish(() => reject(err instanceof Error ? err : new Error(String(err))));
        },
      },
    }).catch((err: unknown) => finish(() => reject(err as Error)));
  });
}