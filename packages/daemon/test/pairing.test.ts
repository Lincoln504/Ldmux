import { test, expect } from "../../testing/src/expect.ts";
import { pairingFor } from "../src/pairing.ts";
import { parsePairingLink } from "../../protocol/src/pairing.ts";
import { encodeQr } from "../src/qr.ts";

const token = "0123456789abcdef".repeat(4);
const live = { hostname: "100.127.119.54", port: 7625, token, listening: true };

test("the pairing is the address the daemon listens on, not a guess", () => {
  const { pairing, link } = pairingFor(live, "debian");
  expect(pairing).toEqual({ host: "100.127.119.54:7625", token, name: "debian" });
  expect(parsePairingLink(link)).toEqual(pairing);
});

test("an IPv6 tailnet address is bracketed", () => {
  expect(pairingFor({ ...live, hostname: "fd7a:115c:a1e0::5" }, "d").pairing.host).toBe("[fd7a:115c:a1e0::5]:7625");
});

test("a daemon on loopback cannot be paired with, and says what to set", () => {
  expect(() => pairingFor({ ...live, hostname: "127.0.0.1" }, "d")).toThrow(/LDMUX_REMOTE_HOST/);
});

test("a daemon whose WebSocket is not up yet says so rather than printing 127.0.0.1:0", () => {
  expect(() => pairingFor({ hostname: "127.0.0.1", port: 0, token, listening: false }, "d")).toThrow(/not up yet/);
});

test("a long or odd hostname is trimmed to what the link allows", () => {
  const { pairing } = pairingFor(live, "x".repeat(80) + "\n");
  expect(pairing.name).toBe("x".repeat(64));
});

test("the deployed shape of link fits a version-6 code", () => {
  // The widest IPv4 tailnet address, the widest port, and a typical name.
  const { link } = pairingFor({ ...live, hostname: "100.127.255.254", port: 65535 }, "debian");
  expect(encodeQr(link).version).toBeLessThanOrEqual(6);
});
