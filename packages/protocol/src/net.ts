/**
 * Which addresses ldmux may listen on and accept connections from.
 *
 * "Tailnet only" (AGENTS.md) is a rule about addresses, so it is written down as
 * one: loopback, or an address Tailscale assigns — `100.64.0.0/10` (the CGNAT
 * range Tailscale allocates IPv4 from) and `fd7a:115c:a1e0::/48` (its IPv6
 * prefix). Anything else is a LAN or the internet, and the daemon neither binds
 * it nor answers it.
 *
 * Literal addresses only. A name would have to be resolved to be checked, and a
 * name that resolves differently tomorrow is not a rule; the deployment names the
 * tailnet address itself (`LDMUX_REMOTE_HOST`). `localhost` is the one name
 * accepted, because it is loopback by definition.
 */

import { BlockList, isIPv4, isIPv6 } from "node:net";

const LOOPBACK = new BlockList();
LOOPBACK.addSubnet("127.0.0.0", 8, "ipv4");
LOOPBACK.addAddress("::1", "ipv6");

const TAILNET = new BlockList();
TAILNET.addSubnet("100.64.0.0", 10, "ipv4");
TAILNET.addSubnet("fd7a:115c:a1e0::", 48, "ipv6");

export type AddressKind = "loopback" | "tailnet" | "other";

/**
 * Classify one address as a socket reports it.
 *
 * An IPv4 address may arrive IPv6-mapped (`::ffff:100.64.1.2`) from a dual-stack
 * socket; it is judged as the IPv4 address it is. IPv6 zone ids (`fe80::1%eth0`)
 * are never tailnet or loopback, and are rejected as `other`.
 */
export function classifyAddress(raw: string): AddressKind {
  let addr = raw.trim();
  if (addr === "localhost") return "loopback";
  if (addr.startsWith("[") && addr.endsWith("]")) addr = addr.slice(1, -1);
  const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/i.exec(addr);
  if (mapped) addr = mapped[1]!;
  const family = isIPv4(addr) ? "ipv4" : isIPv6(addr) ? "ipv6" : null;
  if (family === null) return "other";
  if (LOOPBACK.check(addr, family)) return "loopback";
  if (TAILNET.check(addr, family)) return "tailnet";
  return "other";
}

/** May the daemon listen here, and may a peer at this address talk to it? */
export function isTailnetOrLoopback(addr: string): boolean {
  return classifyAddress(addr) !== "other";
}
