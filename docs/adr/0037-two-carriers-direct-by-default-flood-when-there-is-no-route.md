# 0037 — Two carriers: direct by default, a flood when there is no route

**Status:** accepted (2026-11-10). **Under revision, 2026-11-11:** the two-carrier model (`RAW_CUSTOM` direct + `REQ` `LDMR` flood) stands, but the capacity claim in *Consequences* below omitted the flood's growing travel path `P_arrival` and the companion's 3-byte `0x88` receive-log header (`R <= 161 - S - P_arrival`, `RC-6`), and adopting a flood's reversed travel path as `proven = true` conflated inbound arrival with outbound proof. Corrected in [`docs/RADIO-PLAN.md`](../RADIO-PLAN.md) §4–§5 and [`docs/RADIO-AUDIT-REVIEW.md`](../RADIO-AUDIT-REVIEW.md).

## Context

A session record is a `PAYLOAD_TYPE_RAW_CUSTOM` direct packet. The firmware delivers that type
**only on a direct route** (`Mesh.cpp`, `case PAYLOAD_TYPE_RAW_CUSTOM` accepts only `isRouteDirect()`)
and a repeater never forward-routes one. So every record depends on a path this end believes in, and
every wait in the session is timed against that path's length.

At the edge of coverage the path cannot always be found. Measured on 2026-11-10 with the phone's own
receiver in the middle of a city: the nearest repeaters arrived at −107 to −124 dBm, which is the
sensitivity floor for 910.525 MHz / SF7 / 62.5 kHz, while the same receiver sees −88 dBm at home. In
that state the phone sent route requests for four minutes, heard almost nothing, and **the host never
received one of them** — so the link had no way to bootstrap at all. Home worked because a zero-hop
direct packet needs no chain.

The alternative payload types are not a free choice. `PAYLOAD_TYPE_GRP_DATA` (0x06) is floodable and
carries 165 bytes, but neither client can send it: the phone's library has
`SEND_CHANNEL_DATAGRAM(0x3E)` as a constant with no builder and no send path, and the host crate has
nothing either. Adding it would mean writing a channel-keyed encryption layer, a channel provisioning
step on both radios, and a new command on both ends — for a carrier we only need as a fallback.

## Decision

**The record stays the same; only the packet around it changes.** A sealed record rides a
`PAYLOAD_TYPE_REQ` flood when no fresh route is known, and a `RAW_CUSTOM` direct packet otherwise.

- Wrapper: `[header][path byte = 0][dest hash 1][source hash 1]["LDMR" 4][nonce 4][record]`, a
  `REQ` flood (header `0x01`) or a scoped `TRANSPORT_FLOOD` (header `0x00`) when the radio has a
  region configured. Same builder shape as the route request and reply that already cross this mesh.
- The **nonce is outside the sealed record and fresh per send**, because MeshCore dedupes on
  `SHA256(payload_type ‖ payload)` and a retransmission must still be a different packet.
- **Who sent it is not carried.** The record's own AEAD decides that; a flood from a stranger costs
  one authentication check to refuse.
- **When each is used.** The phone floods a record when its route is not fresh. The host, with no
  path, sends the first reply as a plain zero-hop direct packet — cheap, and correct for a
  neighbour — and escalates to a flood only if the peer is still re-handshaking after that. Any
  record that arrives resets the escalation, so a link that works never pays for a flood.
- **A flooded record repairs the route in both directions.** The flood's travel path is the sender's
  chain reversed, so the receiver adopts it and marks it proven, because the record itself just
  crossed it.

Rejected alternatives:

- **`GRP_DATA` and a private channel** — the more capable carrier (165 bytes, channel-key encrypted,
  floodable with an explicit path option), but neither client can send it today and it adds a second
  encryption layer under our own. Kept as the upgrade path if a fallback ever needs to carry more
  than one record at a time.
- **Flooding every record** — a flood is forwarded by every repeater in range. At 172 bytes and 602 ms
  of airtime per record that is the whole city's channel for every frame of a terminal session.
- **Rotating the host's candidate chains instead** — cheaper per record, and still worth having, but
  it can only find a chain that exists; it cannot bootstrap one where the phone hears nothing usable.

## Consequences

- The link can come up with no route at all: the handshake crosses by flood in both directions, and
  the first flooded record teaches each end the chain to the other.
- A flood costs every repeater in range the transmission of one record, so it is used only when the
  route is unknown. A working link never pays it.
- Record capacity on the flood carrier is *larger* than on a long path (`172 − 10` versus
  `172 − path bytes`), so a record that fits a direct path always fits the flood.
- The session's replay window and dedup do the work that a path's ordering used to imply: two copies
  of one flooded record arriving by different chains are one record.
