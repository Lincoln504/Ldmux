/**
 * What `ldmux code` and `ldmux pair` show: the pairing for the address this
 * daemon actually listens on (ADR-0016, ADR-0017).
 *
 * Built from `remote_info`, never from a guess about how the machine is exposed.
 * The previous version assembled a `tailscale serve` URL and a deep link with port
 * 443 for an architecture that was not deployed, so the one output meant to connect
 * a phone pointed it at a port where nothing listened.
 */

import { hostname } from "node:os";
import { classifyAddress } from "../../protocol/src/net.ts";
import { pairingLink, type Pairing } from "../../protocol/src/pairing.ts";

export interface ListenInfo {
  hostname: string;
  port: number;
  token: string;
  listening: boolean;
}

/** The pairing a phone should use, or an error saying what to fix. */
export function pairingFor(info: ListenInfo, name: string = hostname()): { pairing: Pairing; link: string } {
  if (!info.listening) {
    throw new Error("the daemon's WebSocket is not up yet (is tailscale up?); try again in a few seconds");
  }
  if (classifyAddress(info.hostname) === "loopback") {
    throw new Error(
      `the daemon listens on ${info.hostname} only, which a phone cannot reach: ` +
        "set LDMUX_REMOTE_HOST to this machine's tailnet address (`tailscale ip -4`) in deploy/ldmux.service, then `just install`",
    );
  }
  const address = info.hostname.includes(":") ? `[${info.hostname}]` : info.hostname;
  const pairing: Pairing = {
    host: `${address}:${info.port}`,
    token: info.token,
    // What the app calls the machine. Trimmed to what the link allows rather than
    // refused: a long hostname is not a reason not to pair.
    name: name.replace(/[\u0000-\u001f\u007f]/g, "").slice(0, 64),
  };
  return { pairing, link: pairingLink(pairing) };
}
