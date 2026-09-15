import { test, expect } from "../../testing/src/expect.ts";
import { buildPairInfo } from "../src/pairing.ts";

test("pair builds the https url, local url, deep link and serve command", () => {
  const p = buildPairInfo({ host: "debian.tail496756.ts.net", port: 7623, token: "abc123" });
  expect(p.url).toBe("https://debian.tail496756.ts.net/?token=abc123");
  expect(p.localUrl).toBe("http://127.0.0.1:7623/?token=abc123");
  expect(p.deepLink).toBe("ldmux://pair?host=debian.tail496756.ts.net&port=443&token=abc123");
  expect(p.serveCommand).toBe("tailscale serve --bg --https=443 http://127.0.0.1:7623");
});

test("a non-443 public port is carried in the url and deep link", () => {
  const p = buildPairInfo({ host: "100.127.119.54", port: 7623, token: "t", publicPort: 8443 });
  expect(p.url).toBe("https://100.127.119.54:8443/?token=t");
  expect(p.deepLink).toBe("ldmux://pair?host=100.127.119.54&port=8443&token=t");
  expect(p.serveCommand).toBe("tailscale serve --bg --https=8443 http://127.0.0.1:7623");
});

test("the token is url-encoded", () => {
  const p = buildPairInfo({ host: "h", port: 1, token: "a b&c" });
  expect(p.url).toBe("https://h/?token=a%20b%26c");
  expect(p.deepLink).toContain("token=a%20b%26c");
});
// ── tailnetHost/expose against a stub `tailscale` ─────────────────────────
// These parse real command output, so they are tested with a fake binary on
// PATH rather than skipped when the machine's tailscale is absent or logged out.
import { afterAll, beforeEach } from "../../testing/src/expect.ts";
import { chmodSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expose, tailnetHost } from "../src/pairing.ts";

const binDir = mkdtempSync(join(tmpdir(), "ldmux-faketailscale-"));
const realPath = process.env.PATH ?? "";

function stubTailscale(script: string): void {
  const path = join(binDir, "tailscale");
  writeFileSync(path, `#!/bin/sh\n${script}\n`);
  chmodSync(path, 0o755);
  process.env.PATH = `${binDir}:${realPath}`;
}

beforeEach(() => {
  process.env.PATH = realPath;
});

afterAll(() => {
  process.env.PATH = realPath;
  rmSync(binDir, { recursive: true, force: true });
});

test("tailnetHost reads Self.DNSName and strips the trailing dot", async () => {
  stubTailscale(`echo '{"Self":{"DNSName":"debian.tail496756.ts.net.","TailscaleIPs":["100.127.119.54"]}}'`);
  expect(await tailnetHost()).toBe("debian.tail496756.ts.net");
});

test("tailnetHost falls back to the 100.x address when there is no DNS name", async () => {
  stubTailscale(`echo '{"Self":{"TailscaleIPs":["100.127.119.54","fd7a::1"]}}'`);
  expect(await tailnetHost()).toBe("100.127.119.54");
});

test("tailnetHost is undefined when tailscale fails or returns junk", async () => {
  stubTailscale(`exit 1`);
  expect(await tailnetHost()).toBeUndefined();
  stubTailscale(`echo 'not json'`);
  expect(await tailnetHost()).toBeUndefined();
  stubTailscale(`echo '{}'`);
  expect(await tailnetHost()).toBeUndefined();
  // A missing binary must be undefined too, not a thrown error.
  process.env.PATH = join(binDir, "definitely-empty");
  expect(await tailnetHost()).toBeUndefined();
});

test("expose runs tailscale serve with the loopback target and reports success", async () => {
  stubTailscale(`echo "$@" > ${binDir}/args.txt; exit 0`);
  expect(await expose({ port: 7623 })).toEqual({ ok: true });
  const args = readFileSync(join(binDir, "args.txt"), "utf8").trim();
  expect(args).toBe("serve --bg --https=443 http://127.0.0.1:7623");
});

test("expose surfaces tailscale's stderr on failure", async () => {
  stubTailscale(`echo "serve: not logged in" >&2; exit 1`);
  const res = await expose({ port: 7623, publicPort: 8443 });
  expect(res.ok).toBe(false);
  expect(res.detail).toContain("not logged in");
});
