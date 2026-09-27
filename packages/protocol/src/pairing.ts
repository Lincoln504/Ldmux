/**
 * The pairing link: everything a phone needs to reach this daemon, in one URL.
 *
 *   ldmux://pair?v=1&host=100.127.119.54:7625&token=<64 hex>&name=debian
 *
 * `ldmux code` shows it as a QR code; the app scans it (or the system camera opens
 * it) and stores `host` and `token` exactly as they are here. It is a contract
 * between the CLI and the app, so it is pinned from both sides by one fixture
 * (`test/fixtures/pairing-links.json`, read by this package's tests and by the
 * app's `PairingTest`).
 *
 * - `host` is the address the daemon *listens on* (`remote_info`), as
 *   `address:port`, IPv6 in brackets. It must be tailnet or loopback (ADR-0016):
 *   a link to anywhere else is refused by the app, so a crafted QR cannot point a
 *   phone at someone else's machine on the internet.
 * - `token` is the bearer token, 64 lowercase hex characters, as `auth.ts`
 *   generates it.
 * - `name` is the machine's name, for the app to say what it is paired with. It is
 *   optional, and at most 64 characters.
 * - `v` is this format's version. A link with a version the app does not know is
 *   refused rather than guessed at.
 */

import { isTailnetOrLoopback } from "./net.ts";

export const PAIRING_VERSION = 1;

export interface Pairing {
  host: string;
  token: string;
  name: string;
}

const TOKEN = /^[0-9a-f]{64}$/;
const NAME_MAX = 64;

/** The link for [p]. Throws on a pairing the app would refuse, so the CLI never shows one. */
export function pairingLink(p: Pairing): string {
  const problem = pairingProblem(p);
  if (problem) throw new Error(problem);
  const fields: [string, string][] = [["v", String(PAIRING_VERSION)], ["host", p.host], ["token", p.token]];
  if (p.name) fields.push(["name", p.name]);
  // `:` is legal in a query and every host has one, so it is left as itself: four
  // bytes a code does not have to carry, which is the difference between version 6
  // and 7 — 41 modules or 45, two terminal rows.
  const enc = (v: string) => encodeURIComponent(v).replaceAll("%3A", ":");
  return `ldmux://pair?${fields.map(([k, v]) => `${k}=${enc(v)}`).join("&")}`;
}

/** The pairing in [link], or null if it is not a valid ldmux pairing link. */
export function parsePairingLink(link: string): Pairing | null {
  let url: URL;
  try {
    url = new URL(link.trim());
  } catch {
    return null;
  }
  if (url.protocol !== "ldmux:" || url.hostname !== "pair") return null;
  if (url.searchParams.get("v") !== String(PAIRING_VERSION)) return null;
  const p: Pairing = {
    host: url.searchParams.get("host") ?? "",
    token: url.searchParams.get("token") ?? "",
    name: url.searchParams.get("name") ?? "",
  };
  return pairingProblem(p) ? null : p;
}

/** Split `address:port` (IPv6 bracketed). Null when it is not that shape. */
export function splitHost(host: string): { address: string; port: number } | null {
  const m = /^(\[[0-9a-fA-F:.]+\]|[^:[\]]+):(\d{1,5})$/.exec(host);
  if (!m) return null;
  const port = Number(m[2]);
  if (port < 1 || port > 65535) return null;
  return { address: m[1]!, port };
}

function pairingProblem(p: Pairing): string | null {
  const hp = splitHost(p.host);
  if (!hp) return `pairing host must be address:port, got ${JSON.stringify(p.host)}`;
  if (!isTailnetOrLoopback(hp.address)) return `pairing host must be a tailnet or loopback address, got ${hp.address}`;
  if (!TOKEN.test(p.token)) return "pairing token must be 64 lowercase hex characters";
  if (p.name.length > NAME_MAX || /[\u0000-\u001f\u007f]/.test(p.name)) return "pairing name must be at most 64 printable characters";
  return null;
}
