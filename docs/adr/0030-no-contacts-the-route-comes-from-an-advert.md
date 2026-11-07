# ADR-0030 — ldmux keeps no contacts; a route comes from an advert

**Status:** Accepted (2026-11-07).

- **Date:** 2026-11-07
- **Related:** `docs/ARCHITECTURE.md` §11, `crates/companion/src/codec.rs` (`GET_ADVERT_PATH`,
  `SET_PATH_HASH_MODE`), `crates/daemon/src/mesh_serial.rs` (`PeerPath`, `refresh_peer_route`),
  `packages/mobile/.../mesh/AndroidRadioLink.kt` (`contactPath`, `advertPath`, `preferHeard`),
  ADR-0029 (the session rides `RAW_CUSTOM`)

## Context

MeshCore's companion radio keeps a **contact list**: an advert adds a node, the list survives
reboots, and it is what the MeshCore app shows as "contacts" with last-heard time, hop count and
signal. Reading the firmware rather than the guides (`examples/companion_radio/MyMesh.cpp`,
`src/helpers/BaseChatMesh.*`, `NodePrefs.h`) gives the shape:

- **Capacity.** `MAX_CONTACTS` is 100 on a companion build (`MyMesh.h:59`) and 32 in the base class
  (`BaseChatMesh.h:38`); `CMD_DEVICE_QUERY` reports `MAX_CONTACTS / 2`, so a client is told 50
  (`MyMesh.cpp:1036`).
- **Discovery.** Every advert heard is a candidate. `manual_add_contacts & 1 == 0` means
  "auto-add everything"; otherwise an `autoadd_config` bitmask decides per type —
  `OVERWRITE_OLDEST` 0x01, `CHAT` 0x02, `REPEATER` 0x04, `ROOM_SERVER` 0x08, `SENSOR` 0x10
  (`MyMesh.cpp:142-146`, `:311-330`), set and read by `CMD_SET_AUTOADD_CONFIG` 58 / `CMD_GET_AUTOADD_CONFIG` 59.
- **Eviction.** With `OVERWRITE_OLDEST`, a full table deletes the oldest **non-favourite** contact
  and pushes `PUSH_CODE_CONTACT_DELETED` 0x8F; with nothing evictable it pushes
  `PUSH_CODE_CONTACTS_FULL` 0x90. Writes are debounced 5 s (`LAZY_CONTACTS_WRITE_DELAY`), so a
  contact learned a moment ago is not durable yet.
- **A contact is 152 bytes on disk**, with a 64-byte `out_path`, `out_path_len`, `lastmod` and
  `last_advert_timestamp` (`DataStore.cpp:230` region). `CMD_ADD_UPDATE_CONTACT` 0x09 writes the same
  fields over the wire: key 32, type 1, flags 1, path length 1, path 64, name 32, advert timestamp 4,
  optional position and `lastmod`.
- **What only a contact unlocks.** `CMD_SEND_PATH_DISCOVERY_REQ` 52 resolves the target with
  `lookupContactByPubKey` and answers `ERR_CODE_NOT_FOUND` without one (`MyMesh.cpp:1609-1650`).
  It is "a special case of flood + Telemetry req", and the responder answers only when its own
  telemetry permission allows the BASE class (`MyMesh.cpp:629-655`) — `telemetry_mode_base` defaults
  to **deny** (`NodePrefs.h:24`). A successful exchange returns the path in both directions and
  pushes `PUSH_CODE_PATH_DISCOVERY_RESPONSE` 0x8D, and later path changes push
  `PUSH_CODE_PATH_UPDATED` 0x81.
- **What a contact is for, in MeshCore's own product.** Direct messages (their own static-static
  ECDH, no rekey — ADR-0029 says why we do not use them), node type and position, last-heard and
  hop/SIG data in a list, and remote CLI access to a trusted node.

## Decision

**ldmux keeps no contact list, and does not use the contact-dependent route machinery.** Both ends
learn the route they need from the **advert-path cache** (`onDiscoveredContact` → `advert_paths[]`,
read back with `CMD_GET_ADVERT_PATH` 0x2A), which needs no contact entry on either radio and works
on firmware as shipped.

What that buys, in the terms above:

- **No capacity to manage.** The cache is 16 slots, RAM-only and evicts by age; it refills from the
  next advert. There is no table to fill, no auto-add policy to get wrong, and no "contacts full"
  state to surface, because nothing is persisted.
- **No eviction protocol to mirror.** 0x8F and 0x90 exist only to keep a client's mirror of a
  persisted list consistent. A cache that misses simply misses, and both ends re-ask on their
  cadence.
- **No stale records.** A `lastmod`/`out_path` pair can describe where a peer *used to be* for as
  long as nothing replaces it, and on a companion nothing does: a path return is the only writer, and
  a companion never receives one without a contact. That is why the phone prefers a freshly heard
  advert path over a stored contact path at all (`RadioRoute.preferHeard`).
- **No permission negotiation.** A path return needs the responder to allow BASE telemetry to that
  contact; the default is deny, and asking the operator to open telemetry so a terminal can find its
  host is a security decision taken for a routing convenience.
- **No scope creep.** DMs, node roles, positions, maps and remote CLI are what a contact list is
  for. ldmux carries its own Noise session over `RAW_CUSTOM` (ADR-0029) and its own control channel;
  none of those features are on its path.

One thing is still read: at connect the phone asks for the contact dump
(`CMD_GET_CONTACTS`) and uses the **host's** entry if one exists, because `out_path` is already in
sending order and costs nothing to prefer when the cache cannot answer. That is a read of data the
radio already has; it creates no state here and writes none there.

## Consequences

- On a crowded channel the 16-slot cache can miss for a peer heard a minute ago. The answer is the
  existing one: re-read on the cadence, and a failed read never invalidates a working route
  (`ARCHITECTURE.md` §11). The first sign of this being a real limit is a link that only comes up
  after several attempts in a dense area **while the peer's adverts are arriving**.
- If that is ever observed, the upgrade is ADR-sized and has an order: add the peer as a contact
  (0x09), let the radio answer a path discovery (52 → 0x8D), and consume `PUSH_CODE_PATH_UPDATED`
  (0x81) instead of polling. Its prerequisites and hazards are the four bullets above, and it is
  only worth paying when the cache demonstrably misses.
- Route quality today is one-directional per flood: each end learns the other's path from that
  end's advert. The host forces an advert on a route hint precisely because of that, bounded to one
  per three minutes.
