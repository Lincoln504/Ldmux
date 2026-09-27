import { test, expect } from "../../testing/src/expect.ts";
import { classifyAddress, isTailnetOrLoopback } from "../src/net.ts";

/**
 * "Tailnet only" as data. The edges are the point: the first and last address of
 * each range, one past each end, and the shapes a socket actually reports.
 */

test("loopback, in every form a socket reports it", () => {
  for (const a of ["127.0.0.1", "127.255.255.254", "::1", "[::1]", "::ffff:127.0.0.1", "localhost"]) {
    expect(`${a}:${classifyAddress(a)}`).toBe(`${a}:loopback`);
  }
});

test("the tailnet is 100.64.0.0/10 and fd7a:115c:a1e0::/48, edges included", () => {
  for (const a of ["100.64.0.0", "100.127.119.54", "100.127.255.255", "::ffff:100.100.1.2", "fd7a:115c:a1e0::1", "fd7a:115c:a1e0:ffff::1"]) {
    expect(`${a}:${classifyAddress(a)}`).toBe(`${a}:tailnet`);
  }
});

test("one past either end of the tailnet range, a LAN, the internet and any name are not allowed", () => {
  for (const a of [
    "100.63.255.255",
    "100.128.0.0",
    "0.0.0.0",
    "::",
    "192.168.1.5",
    "10.0.0.1",
    "8.8.8.8",
    "fd7a:115c:a1e1::1",
    "fe80::1%eth0",
    "debian.tail496756.ts.net",
    "",
    "not an address",
  ]) {
    expect(`${a}:${isTailnetOrLoopback(a)}`).toBe(`${a}:false`);
  }
});
