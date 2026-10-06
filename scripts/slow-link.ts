/**
 * A slow, laggy link between a client and the daemon, for trying the app on
 * "bad mobile data" without root or `tc netem`.
 *
 *   node scripts/slow-link.ts --listen 100.x.y.z:7626 --to 100.x.y.z:7625 \
 *        --delay-ms 300 --kbps 160
 *
 * Point the phone at the listen address (settings → host). Every byte is held for
 * `--delay-ms` in each direction and the stream is paced to `--kbps` kilobits per
 * second each way, which is roughly a weak 3G link at the defaults.
 * It is a protocol-agnostic TCP proxy (it never parses the stream), so it works
 * in front of the Rust daemon's WebSocket listener unchanged. Needs Node. TCP still
 * delivers everything in order: this models latency and bandwidth, not loss —
 * loss on a real mobile link shows up to a TCP client as exactly this, stalls.
 */

import { createConnection, createServer, type Socket } from "node:net";

function arg(name: string, fallback: string): string {
  const i = process.argv.indexOf(name);
  return i >= 0 && process.argv[i + 1] ? process.argv[i + 1]! : fallback;
}

function hostPort(s: string): { host: string; port: number } {
  const at = s.lastIndexOf(":");
  return { host: s.slice(0, at), port: Number(s.slice(at + 1)) };
}

const listen = hostPort(arg("--listen", "127.0.0.1:7626"));
const target = hostPort(arg("--to", "127.0.0.1:7625"));
const delayMs = Number(arg("--delay-ms", "300"));
const bytesPerSec = (Number(arg("--kbps", "160")) * 1000) / 8;

/**
 * One direction of the link: a queue released no earlier than `delayMs` after
 * arrival, and no faster than the bandwidth allows.
 */
function pipeSlow(from: Socket, to: Socket, label: string): void {
  const queue: { at: number; data: Buffer }[] = [];
  let busyUntil = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;

  const pump = (): void => {
    timer = null;
    const now = Date.now();
    while (queue.length > 0 && queue[0]!.at <= now && busyUntil <= now) {
      const chunk = queue.shift()!;
      to.write(chunk.data);
      busyUntil = Math.max(busyUntil, now) + (chunk.data.length / bytesPerSec) * 1000;
    }
    if (queue.length > 0 && timer === null) {
      timer = setTimeout(pump, Math.max(1, Math.max(queue[0]!.at, busyUntil) - now));
    }
  };

  from.on("data", (data: Buffer) => {
    // Split large reads so one burst does not jump the pacing.
    for (let i = 0; i < data.length; i += 1024) {
      queue.push({ at: Date.now() + delayMs, data: data.subarray(i, i + 1024) });
    }
    if (timer === null) pump();
  });
  from.on("close", () => {
    const drain = setInterval(() => {
      if (queue.length === 0) {
        clearInterval(drain);
        to.end();
      }
    }, 50);
  });
  from.on("error", () => to.destroy());
  void label;
}

createServer((client) => {
  const upstream = createConnection(target);
  pipeSlow(client, upstream, "up");
  pipeSlow(upstream, client, "down");
  const done = () => {
    client.destroy();
    upstream.destroy();
  };
  client.on("error", done);
  upstream.on("error", done);
}).listen(listen.port, listen.host, () => {
  console.log(
    `slow-link ${listen.host}:${listen.port} -> ${target.host}:${target.port}, ` +
      `${delayMs} ms each way, ${bytesPerSec * 8 / 1000} kbps each way`,
  );
});
