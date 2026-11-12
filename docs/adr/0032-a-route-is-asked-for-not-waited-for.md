# ADR-0032 — A route is asked for, not waited for

**Status:** Accepted (2026-11-08). **Partially superseded by ADR-0035 and revised 2026-11-11:** overlapping `canSendRaw()` with `GET_CONTACT_BY_KEY` (`0x1E`) and `GET_ADVERT_PATH` (`0x2A`) caused `ERR_CODE_NOT_FOUND` replies from those queries to settle `pendingRaw` (`sendRawPacket`) as `ErrNotFound`, wiping the learned route and killing the session (`RC-2` in [`docs/RADIO-AUDIT-REVIEW.md`](../RADIO-AUDIT-REVIEW.md)); and running `LDRQ` before `msg1` on connect caused `LDRR` and `msg1` to collide (`RC-1`). See [`docs/RADIO-PLAN.md`](../RADIO-PLAN.md) §3.2 and §5.1.

- **Date:** 2026-11-08
- **Related:** ADR-0030 (no contacts; a route comes from an advert), ADR-0029 (the session rides
  `RAW_CUSTOM`), `docs/ARCHITECTURE.md` §11,
  `crates/companion/src/codec.rs` (`ROUTE_FLOOD_HEADER`, `build_route_request`,
  `build_route_reply`, `parse_route_request`, `parse_route_reply`),
  `crates/daemon/src/mesh_serial.rs` (`send_route_request`, `send_route_reply`, `route_reply`),
  `packages/mobile/.../mesh/RadioRoute.kt` (`routeRequest`, `routeReply`, `heardAdvert`)

## Context

ADR-0030 gave ldmux a route with no contact list: both ends learn it from a **flooded advert**.
That works, and it was proven at home. It failed in a city, and the measurements say why.

**An advert cannot cross a public mesh.** A repeater drops an advert once it has travelled
`flood_max_advert` hops, which is **8 by default** (`examples/simple_repeater/MyMesh.cpp:907`,
added upstream in v1.16.0 as PR #2702). Measured on this machine's local mesh on 2026-11-08 with a
receive-log capture of the host radio: adverts arrive at **4, 5, 6 and 8** hops, while `REQ` floods
arrive at **18-29** hops in the same window. Two radios more than eight hops apart therefore cannot
exchange adverts at all, so a route learned only from adverts is not merely slow — it does not
exist. The guides make the same point from the other direction: repeaters are told to advertise
every 47 hours, companions never.

**And even within reach it was slow, in four compounding ways.**

- The host advertised every **300 s** (`LDMUX_MESH_ADVERT_SECS`), so the phone's worst case was a
  five-minute wait for the only packet that could teach it a route.
- The phone reads its radio's advert-path cache on a timer that keyed on *"is a path present"*.
  A phone that had connected once at home holds a zero-hop path, so it answered yes and refreshed
  every **300 s** — it could miss a fresh advert for five minutes after hearing it.
- The phone announced itself only when `quiet` (nothing received for 120 s) and at most every
  **180 s**, so the host usually had nothing to answer.
- The phone's own answer-window gate refused a raw send whenever *any* tracked command was in
  flight, including typed reads that cannot be confused with it. `connect()` reads three of them
  and then sends the route hint and the first handshake message, so both were refused, and a
  refused handshake is not retried until the next ladder rung — 8, 16, 32 s. Measured on the
  device 2026-11-08 at one metre from the host: "handshaking" for 24-29 s.

## Decision

**A route is requested, not waited for.** Both ends can send a **flooded route request** and answer
one, and a route is therefore learned in one round trip at whatever distance the mesh actually
carries.

The wire is two shapes, both in `crates/companion`:

- **Request** —
  `[header 0x01][path byte 0][dest hash 1][src hash 1]["LDRQ"][requester's radio key 32][nonce 4]`.
  **The nonce is what makes a retry possible at all**: MeshCore dedupes on
  `SHA256(payload_type ‖ payload)` in a 160-entry ring with no timeout
  (`Packet::calculatePacketHash`, `SimpleMeshTables`), at every repeater and every receiver, so a
  byte-identical second request dies in the first repeater. The reply echoes the nonce, and the
  route hint carries its own.
  Route `FLOOD`, payload type `REQ` (0), which is the payload type MeshCore's own flooded path
  request uses (`BaseChatMesh::sendRequest` sends a `REQ` flood when the path is unknown). The
  hashes are the firmware's own: the first byte of each public key (`PATH_HASH_SIZE` is 1). The
  tail is ldmux's, and the firmware never reads it: a `REQ` with no matching contact fails its MAC
  and is ignored, while every repeater forwards it by route and hop limit alone.
- **Reply** — a `RAW_CUSTOM` direct record, `["LDRR"][path byte][path]`, carrying **the path the
  replier uses towards the requester**. One rule serves both directions: the reply names the
  sender's sending-order path and the reader reverses it. The requester then holds a route, and the
  replier already learned its own from the request's arriving path.

Why this shape and not the alternatives:

- **Not a contact.** `CMD_SEND_PATH_DISCOVERY_REQ` is the native on-demand mechanism, and it needs
  the peer in the sender's contact table *and* the responder's BASE telemetry permission, which
  defaults to deny (`NodePrefs.h:24`). ADR-0030's reasons still hold.
- **Not a flooded `RAW_CUSTOM`.** The firmware delivers `RAW_CUSTOM` only on a direct route and a
  repeater does not forward one (`src/Mesh.cpp`, `case PAYLOAD_TYPE_RAW_CUSTOM`), so the session's
  own payload type can never be the flood.
- **Not an advert at a faster cadence.** The eight-hop ceiling is not a timer, so no cadence fixes
  it; and an advert is 130 bytes that every repeater in range repeats, which is 1/10 of the
  community's own suggested rate at five minutes.
- **Not a firmware change.** `CMD_SEND_RAW_PACKET` (0x41) takes a whole packet
  (`MyMesh.cpp:2003-2016`), and `Dispatcher::sendPacket` queues it by route and priority with no
  opinion about the payload, so the request is transmitted as built. A stock companion-v1.17.1
  radio can send and receive it.

Adverts stay, at a **community cadence** rather than a ldmux one: one every hour by default
(`LDMUX_MESH_ADVERT_SECS`, `0` turns them off, which is the firmware's own companion default).
They are a maintenance broadcast now, and a fallback path source — not the critical path.

Two more changes come with it, because they were the rest of the delay:

- **A route heard on the air is adopted on the event.** Both ends already receive
  `PUSH_CODE_LOG_RX_DATA` (0x88) for every packet their radio hears, path included. An advert from
  the peer therefore updates the route the moment it arrives instead of at the next poll, and a
  route reply is consumed before the session can mistake it for a record. A route older than two
  minutes is treated as unknown for a *new* connection, so a phone that moved asks instead of
  trusting it.
- **A typed read does not block a raw send.** `MeshcoreSession.canSendRaw()` permits a raw send
  while a command that answers with its own typed frame (contact dump, advert path, device query,
  channel, battery) is in flight, and still refuses it while an `OK`/`ERR`-answered command is
  outstanding. That refusal is what used to cost a handshake record a whole ladder rung.

## Consequences

- **Discovery is one round trip.** A phone that opens the app with no route, or with a stale one,
  floods a request; the host answers direct. Cost at 62.5 kHz/SF7 from the airtime figures
  in `docs/AIRTIME.md`: a 40-byte request and a 40-byte-plus-path reply are ≈0.3 s of air each,
  before any repeater re-transmits them. The host replies
  at most once per 5 s and asks at most once per 10 s, so neither end can be turned into an
  amplifier.
- **Discovery no longer depends on the advert ceiling.** A route works over any distance the mesh
  carries a flood. The advert remains for the mesh's own contact lists, and as the fallback when an
  older pairing has no host radio key.
- **The host remembers the peer's radio key** (`<state>/mesh-peer`), so a restarted daemon can ask
  a phone for a route instead of waiting for its next advert. Without it, the first connection after
  a restart on a phone that had moved could only wait.
- **The airtime policy is band-aware.** One percent inside 869.4-869.65 MHz, where a duty cycle is a
  condition of use; ten percent everywhere else, where there is no rule to comply with and the
  firmware itself would allow fifty. At one percent a single typed command line at 62.5 kHz was the
  whole hour, which made the meter, not the radio, the thing that stopped the session.
  *(Corrected 2026-11-09: the "ten percent everywhere else" was itself a policy cap and was removed
  — the allowance is now the law or nothing, `docs/RELEASE-AUDIT.md` row 6.52. The paragraph above
  stands as the decision of its date.)*
- **One thing is not covered.** A scope-configured mesh (repeaters with a default flood scope and
  `denyf *`) drops unscoped floods, so both the advert and the request die at the first scoped
  repeater. ldmux publishes no scope. A scoped mesh needs `LDMUX_MESH_*`-level scope support, which
  is a separate change; the diagnostics now say when nothing at all arrives from the peer.
