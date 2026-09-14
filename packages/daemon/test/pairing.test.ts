import { test, expect } from "bun:test";
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