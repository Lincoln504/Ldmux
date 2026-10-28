import { connect } from "node:net";

export interface SpeakRequest {
  sessionId: string;
  text: string;
  id?: string;
  final?: boolean;
}

export type SpeakResult = { ok: true; delivered: number; seq: number } | { ok: false; error: string };

/**
 * One `speech` call on the daemon's control socket: one JSON line out, one back
 * (`crates/protocol/src/socket.rs`). Never throws: a daemon that is down, slow or refusing is
 * an answer the caller can show, not an exception in the middle of an agent turn.
 */
export function speak(socketPath: string, req: SpeakRequest, timeoutMs = 3000): Promise<SpeakResult> {
  return new Promise((resolve) => {
    let done = false;
    let buf = "";
    const finish = (r: SpeakResult) => {
      if (done) return;
      done = true;
      clearTimeout(timer);
      sock.destroy();
      resolve(r);
    };
    const sock = connect(socketPath);
    const timer = setTimeout(() => finish({ ok: false, error: "the ldmux daemon did not answer in time" }), timeoutMs);
    sock.setEncoding("utf8");
    sock.on("error", (e) => finish({ ok: false, error: `ldmux daemon: ${e.message}` }));
    sock.on("connect", () => {
      const params: Record<string, unknown> = { sessionId: req.sessionId, text: req.text, final: req.final ?? true };
      if (req.id) params.id = req.id;
      sock.write(JSON.stringify({ id: 1, method: "speech", params }) + "\n");
    });
    sock.on("data", (chunk: string) => {
      buf += chunk;
      const nl = buf.indexOf("\n");
      if (nl < 0) return;
      try {
        const r = JSON.parse(buf.slice(0, nl)) as { ok?: boolean; result?: { delivered?: number; seq?: number }; error?: string };
        if (r.ok) finish({ ok: true, delivered: r.result?.delivered ?? 0, seq: r.result?.seq ?? 0 });
        else finish({ ok: false, error: r.error ?? "the daemon refused" });
      } catch {
        finish({ ok: false, error: "the daemon sent something that is not JSON" });
      }
    });
    sock.on("close", () => finish({ ok: false, error: "the daemon closed the connection without answering" }));
  });
}
