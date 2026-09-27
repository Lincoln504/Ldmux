import { test, expect } from "../../testing/src/expect.ts";
import { readFileSync } from "node:fs";
import { pairingLink, parsePairingLink, type Pairing } from "../src/pairing.ts";

/**
 * The pairing link is a contract between the CLI and the app. The cases live in a
 * fixture both sides read, so the Kotlin parser cannot drift from this one without
 * one of the two suites failing.
 */
const fixture = JSON.parse(readFileSync(new URL("./fixtures/pairing-links.json", import.meta.url), "utf8")) as {
  valid: { link: string; pairing: Pairing }[];
  invalid: string[];
};

test("every valid fixture link parses to its pairing", () => {
  for (const c of fixture.valid) expect(parsePairingLink(c.link)).toEqual(c.pairing);
});

test("every invalid fixture link is refused", () => {
  for (const link of fixture.invalid) expect(`${link} -> ${JSON.stringify(parsePairingLink(link))}`).toBe(`${link} -> null`);
});

test("what the CLI makes, the parser reads back, for every valid pairing", () => {
  for (const c of fixture.valid) expect(parsePairingLink(pairingLink(c.pairing))).toEqual(c.pairing);
});

test("the CLI never makes a link the app would refuse", () => {
  const token = "0".repeat(64);
  expect(() => pairingLink({ host: "192.168.1.5:7625", token, name: "" })).toThrow(/tailnet or loopback/);
  expect(() => pairingLink({ host: "100.127.119.54", token, name: "" })).toThrow(/address:port/);
  expect(() => pairingLink({ host: "100.127.119.54:7625", token: "short", name: "" })).toThrow(/64 lowercase hex/);
});

test("the link is short enough for a small code", () => {
  // A 64-hex token, the longest IPv4 tailnet host and a typical name: this is what
  // decides how many terminal rows `ldmux code` takes.
  const link = pairingLink({ host: "100.127.119.254:65535", token: "f".repeat(64), name: "debian" });
  // 134 bytes is what a version-6 code holds at level L (41 modules, 25 terminal
  // rows with the quiet zone); `ldmux code` asserts the version itself.
  expect(link.length).toBeLessThanOrEqual(134);
});
