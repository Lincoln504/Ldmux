# ADR-0016 — The daemon listens on the tailnet address, and only there

**Status:** Accepted (2026-09-27); supersedes ADR-0005

- **Date:** 2026-09-27
- **Supersedes:** ADR-0005 (loopback bind + `tailscale serve`)
- **Related:** ADR-0017 (QR pairing), `docs/REMOTE.md`, `deploy/ldmux.service`

## Context

ADR-0005 decided on a loopback bind exposed by `tailscale serve` (TLS, MagicDNS).
That is not what runs. The deployed unit sets `LDMUX_REMOTE_HOST=100.x.y.z`
(this machine's tailnet address) and `LDMUX_REMOTE_PORT=7625`, the phone connects
with plain `ws://100.x.y.z:7625/ws`, and `tailscale serve status` reports no
configuration. The docs described one architecture and the machine ran another.

The direct bind was chosen in practice for what it avoids:

- No second moving part. `serve` is state held by tailscaled that has to be
  configured, survives or does not survive restarts, and needs HTTPS certificates
  enabled in the admin console.
- No certificate or MagicDNS dependency on the phone: a literal `100.x` address
  always resolves.

ADR-0005's objection — the tailnet address may not exist yet at boot — is handled:
the daemon retries `EADDRNOTAVAIL` until tailscaled assigns it.

What the direct bind lost was the one guarantee the loopback bind gave for free:
nothing but `tailscale serve` could reach the port. With a configurable bind
address, `0.0.0.0` or a LAN address was accepted without complaint, and no
connection's source was checked. The bearer token was the only gate.

## Decision

- The daemon listens on **its tailnet address or loopback, and nothing else**.
  Tailnet means `100.64.0.0/10` or `fd7a:115c:a1e0::/48`; loopback means
  `127.0.0.0/8`, `::1` or `localhost`. The rule is data in
  `crates/protocol/src/net.rs`.
- It is enforced in three places:
  1. `ldmuxd` (its startup) exits with a message when `LDMUX_REMOTE_HOST` breaks it.
  2. `RemoteServer.start` refuses to listen, for any caller.
  3. Every request and upgrade from a peer outside those ranges gets `403`,
     before the token is looked at.
- Only `/ws` upgrades; any other path is a 404.
- Transport encryption is **WireGuard** (the tailnet). The WebSocket is plain
  `ws://`, so there is no TLS at the application layer. The link never leaves the
  tailnet, and the peer check makes that a property of the daemon, not of the
  deployment.
- The bearer token stays the authentication gate (`crates/daemon/src/auth.rs`).
- `tailscale serve` is not the architecture. `ldmux expose` and the
  `serve`-shaped output of `ldmux pair` went with ADR-0017.

## Consequences

- **+** One process, one address, no certificates. The phone keeps working
  wherever the tailnet does.
- **+** A misconfigured bind address is an error at start, not an exposure.
- **−** No TLS on the socket. A process on another tailnet node could observe
  the traffic only if the tailnet itself were compromised, and that is the trust
  boundary this project already draws ("tailnet only").
- **−** The bind address is machine-specific and lives in `deploy/ldmux.service`.
  If Tailscale re-assigns this machine's address, the unit has to change.
