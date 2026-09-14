/**
 * Pairing: turn the loopback remote endpoint into something a phone can use.
 *
 * ADR-0005 keeps the daemon on loopback and exposes it with `tailscale serve`
 * (TLS + MagicDNS). Pairing hands the client a URL and a deep link carrying the
 * bearer token; the web client already accepts `?token=` and strips it from the
 * address bar.
 */

export interface PairInput {
  /** Tailnet hostname (MagicDNS) or 100.x address. */
  host: string;
  /** Local remote port (the loopback bind). */
  port: number;
  token: string;
  /** Public HTTPS port used by `tailscale serve` (default 443). */
  publicPort?: number;
}

export interface PairInfo {
  /** Browser URL for the phone (https when served by tailscale). */
  url: string;
  /** Local URL (same machine / port-forward). */
  localUrl: string;
  /** Custom-scheme deep link for the future native app. */
  deepLink: string;
  /** Command that exposes the loopback port over the tailnet. */
  serveCommand: string;
}

export function buildPairInfo(input: PairInput): PairInfo {
  const publicPort = input.publicPort ?? 443;
  const token = encodeURIComponent(input.token);
  const host = input.host;
  const authority = publicPort === 443 ? host : `${host}:${publicPort}`;
  return {
    url: `https://${authority}/?token=${token}`,
    localUrl: `http://127.0.0.1:${input.port}/?token=${token}`,
    deepLink: `ldmux://pair?host=${encodeURIComponent(host)}&port=${publicPort}&token=${token}`,
    serveCommand: `tailscale serve --bg --https=${publicPort} http://127.0.0.1:${input.port}`,
  };
}

/** The machine's tailnet hostname, or undefined when tailscale is unavailable. */
export function tailnetHost(): string | undefined {
  try {
    const proc = Bun.spawnSync(["tailscale", "status", "--json"]);
    if (proc.exitCode !== 0) return undefined;
    const status = JSON.parse(proc.stdout.toString()) as {
      Self?: { DNSName?: string; TailscaleIPs?: string[] };
    };
    const dns = status.Self?.DNSName?.replace(/\.$/, "");
    return dns || status.Self?.TailscaleIPs?.[0];
  } catch {
    return undefined;
  }
}

/** Run `tailscale serve` to expose the loopback port (best-effort). */
export function expose(input: { port: number; publicPort?: number }): { ok: boolean; detail?: string } {
  const publicPort = input.publicPort ?? 443;
  const proc = Bun.spawnSync([
    "tailscale",
    "serve",
    "--bg",
    `--https=${publicPort}`,
    `http://127.0.0.1:${input.port}`,
  ]);
  if (proc.exitCode !== 0) return { ok: false, detail: proc.stderr.toString().trim() || "tailscale serve failed" };
  return { ok: true };
}