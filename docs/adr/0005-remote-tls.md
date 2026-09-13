# ADR-0005: Remote access via loopback bind + `tailscale serve`, token + QR pairing

**Status:** Accepted (system Tailscale v1.102.3 present)

## Context

The v0.1 spec left TLS/pairing ambiguous ("`tailscale serve` *or* `tailscale
cert`"; "direct bind + token"). Direct binding to the tailnet IP requires binding
a non-loopback address that can change on Tailscale restart, implies plain `ws://`,
and left token provisioning undefined. Third-party mobile apps sometimes fail to
resolve MagicDNS, so a `100.x` fallback is needed.

## Decision

- Bind the daemon to **loopback** only; expose it with
  `tailscale serve --bg --https=443 http://127.0.0.1:<port>` (TLS + MagicDNS;
  optional `Tailscale-User-Login` identity headers). `tailscale cert` is the
  fallback if `serve` is unavailable.
- **Two gates:** tailnet identity (WireGuard; optional `whois`/identity-header
  allow list) + bearer token. Token stored `0600`, hashed at rest, never plaintext.
- **Pairing:** host displays a **QR code + short code** (deep link
  `ldmux://pair?host=…&port=…&token=…`); manual entry fallback; show the `100.x`
  address for apps where MagicDNS fails.
- **No Funnel** by default. Refuse to bind a public interface.

## Consequences

- **+** No bind-address churn; TLS + stable MagicDNS name for clients.
- **+** Identity headers available without custom OIDC.
- **−** Requires HTTPS certificates enabled in the Tailscale admin console.
- **−** `tailscale serve` must be active (a `systemd --user` unit or `--bg`).