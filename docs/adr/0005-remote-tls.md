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
  allow list) + bearer token. The token is stored **plaintext** at
  `<stateDir>/token` with mode `0600` (state dir `0700`) and compared in constant
  time: another process running as the same user can read it, so it is a gate
  against the network, not against a local user. The identity allow list
  mentioned above is **not implemented**.
- **Pairing:** `ldmux pair` prints an `https://<magicdns-host>/?token=…` URL, a
  local URL, the `ldmux://pair?host=…&port=…&token=…` deep link and the
  `tailscale serve` command. **No QR code is generated** (`qrencode` is not
  installed on this machine) and there is no short code: the URL/deep link is
  the pairing payload, with manual token entry as the fallback.
- **No Funnel** by default. Refuse to bind a public interface.

## Consequences

- **+** No bind-address churn; TLS + stable MagicDNS name for clients.
- **+** Identity headers available without custom OIDC.
- **−** Requires HTTPS certificates enabled in the Tailscale admin console.
- **−** `tailscale serve` must be active (a `systemd --user` unit or `--bg`).