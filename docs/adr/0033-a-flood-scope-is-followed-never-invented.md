# ADR-0033 — A flood scope is followed, never invented

**Status:** Accepted (2026-11-08).

- **Date:** 2026-11-08
- **Related:** ADR-0032 (a route is asked for, not waited for), ADR-0030 (no contacts),
  `docs/ARCHITECTURE.md` §11, `crates/companion/src/codec.rs` (`FloodScope`, `transport_code`,
  `build_route_request_in_scope`, `identify_scope`, `KNOWN_SCOPE_TAGS`),
  `crates/daemon/src/mesh_serial.rs` (`ScopeChoice`),
  `packages/mobile/.../mesh/RadioRoute.kt` (`scopeKey`, `transportCode`, `heardScope`),
  `packages/mobile/.../mesh/AndroidRadioLink.kt` (`radioScope`, `adoptedScope`)

## Context

MeshCore regions are a **flood-forwarding filter and nothing else**. A region is a local record; its
key is the first 16 bytes of `SHA256("#" + name)` for the public hashtag-style regions everybody
uses (`TransportKeyStore::getAutoKeyFor`, `RegionMap::getTransportKeysFor`); a scoped flood carries
the 16-bit `HMAC-SHA-256(key, payload_type ‖ payload)` and nothing else (`TransportKey::calcTransportCode`). A repeater recomputes that code against each region it carries and forwards
only on a match — `RegionMap::findMatch` returns `NULL` otherwise, and `allowPacketForward` then
refuses. **The drop is silent**: no error frame, no counter, no log line in a release build.

Five facts shape the decision, and each one is unfriendly to a UI control:

- **Scope gates floods only.** `allowPacketForward` checks the region when `isRouteFlood()`; a direct
  packet is forwarded on its path alone. So ldmux's session records — every keystroke, every screen —
  cross a scoped mesh whatever the scope, and only *discovery* (the route request, the advert) is
  gated. That is the whole blast radius of getting this wrong.
- **A wrong scope is worse than none.** An unscoped flood is routed to the wildcard `*` entry, whose
  flood flag is **clear by default**, so it is forwarded by every repeater that has not deliberately
  disabled it. A scoped flood is forwarded only by repeaters that carry that exact tag. Naming a
  region nobody configured does not degrade reach; it removes it.
- **A companion starts unscoped.** The firmware's default is a null scope, and the official app
  exposes one only in *Experimental Settings* (v1.43.0+), where the guides tell users to set it. The
  largest US community guide says outright that there is no national tag standard — `us`, `us-co`,
  `colorado`, `pnw` and `bos` all exist — and that only nearby operators agreeing on a name makes it
  work.
- **The community is split on whether unscoped traffic should exist at all.** Some regions run
  `region denyf *`, which drops every unscoped flood; others publish `flood.max.unscoped 12` as the
  compromise; a large US deployment says regions are additive and `*` stays open. A client that
  always scopes breaks on the second kind and gains nothing on the third; a client that never scopes
  is unreachable on the first.
- **The host radio's own answer is the best evidence available**, because that is where the
  operator's app writes it (`region default`) and where a regional firmware build puts a default
  (`DEFAULT_FLOOD_SCOPE_NAME`). `CMD_GET_DEFAULT_FLOOD_SCOPE` (0x40) returns the name **and the
  16-byte key**, which is also the only way a private (`$…`) region's key can be known.

## Decision

**Follow the radio, fall back to what the mesh proves, otherwise stay unscoped. No new setting.**

The order, implemented identically on both ends:

1. **`LDMUX_MESH_SCOPE`** — an operator's explicit pin (`<tag>`, or `off` for "never scope"). Not a
   setting in the UI, and not needed for anything to work; it exists because an operator whose mesh
   is odd should not have to fork the daemon.
2. **The radio's own default scope**, read with `CMD_GET_DEFAULT_FLOOD_SCOPE`. This is the operator's
   own decision, made in the tool that owns it, and it is what the radio's self-adverts already use.
3. **A scope learned from the air.** When a scoped flood is overheard and its code matches one of a
   curated list of published region tags (`KNOWN_SCOPE_TAGS`), that tag is *candidate evidence* —
   with three conditions, because a 16-bit code over ~70 names has a one-in-a-thousand chance of
   matching by accident:
   - it must have travelled **at least one hop**, or nothing was forwarded and the tag says only
     that an adjacent radio used it;
   - the **same name must match two different payloads** before it is adopted;
   - and the radio is told only once something **answers under it** — a request that comes back is
     the proof that the region is carried, and a wrong scope in flash is the one state in which
     discovery cannot work at all.

   Only while the radio has none: an explicit choice is never overwritten.
4. **None.** Unscoped floods, which most meshes forward and which is where every MeshCore client
   starts.

The scope is then applied to **our own raw floods** — the route request — by computing the transport
code ourselves (`build_route_request_in_scope`). Nothing is written to the radio except in case 3,
and the path-hash width, the tuple and the power are already handled the same way: read what the
radio says, write only what is ours to decide.

**Nothing is shown as a control.** `ldmux mesh status` prints one read-only line
(`scope us-co (floods are scoped to this region)` or `scope none (floods are unscoped…)`) because a
missing route has no other symptom, and the app logs what its radio reports. There is no scope
setting in either UI, because the two cases that matter — the operator set one, or the mesh proves
one — are already handled, and a third control would only invite the failure this ADR is about.

## Consequences

- **Discovery works on a mesh that denies unscoped floods** without any configuration, as long as the
  operator's own app set a scope on either radio or the tag is one of the published ones.
- **Nothing changes on a mesh that forwards unscoped floods** (such as this machine's local mesh,
  measured 2026-11-08: unscoped `REQ` floods at 18-29 hops). No scope is invented, so no reach is
  lost.
- **An unknown tag leaves us unscoped**, and that is the honest outcome: the name is never on the
  air, so a 16-bit code cannot be reversed. The log says a scoped mesh was seen without naming it,
  and the operator's own app is where the tag can be read and set.
- **Two ends on different scopes still exchange records** once a route exists (direct packets are not
  gated) but can never discover each other. That is why `mesh status` prints the line and the app logs
  what its radio reports. **The pairing code does not carry a scope**: the phone's radio is the one
  the operator's MeshCore app manages, so a scope set anywhere is set there first, and a code field
  nothing read was removed rather than carried.
- **Two dependencies join `crates/companion`**: `hmac` and `sha2`, the RustCrypto crates
  `crates/session` already uses for the handshake tag. Both are pure Rust — no C, no `ring`, nothing
  the deny list forbids — and the alternative was reimplementing HMAC-SHA-256 by hand to avoid
  naming them. The app's side uses `javax.crypto`, which is in the platform.
