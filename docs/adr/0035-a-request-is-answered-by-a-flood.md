# 0035 — A request is answered by a flood, because the answer is the only thing that has to arrive

- **Status:** accepted
- **Date:** 2026-11-09
- **Supersedes part of:** 0032 (which chose a *direct* reply)

## Context

ADR-0032 made discovery a flooded `REQ` ("`LDRQ`") answered by a **direct** reply carrying the
path. The reasoning was that a request has to cross repeaters while a reply only has to come back,
and a direct reply costs one transmission rather than one per repeater in range.

The first real test at range broke it. On a single-repeater link at −11 dB SNR, the phone sent a
request every ten seconds for ten minutes; the daemon's log shows the requests arriving and the
replies being sent (`route reply sent ok=true`), and the phone's handshake never completed. Two
handshakes were accepted in that window and both sessions then died, because a session record is
direct-only and goes down the same chain that the reply had just failed to cross.

Two firmware facts decide the design, both from the MeshCore tree:

- **`PAYLOAD_TYPE_RAW_CUSTOM` is delivered only on a direct route** and repeaters do not forward it
(`src/Mesh.cpp:281-287`). Every ldmux session record is a raw custom packet, so **session data can
never be flooded** — and it must not be, because flooding a 172-byte record through twenty repeaters
is twenty times the air.
- **The flood vehicles are not equal.** An advert is dropped by a repeater after `flood_max_advert`
hops, which is **eight** by default; a `REQ` flood travels `flood_max` and `flood_max_unscoped`,
both **64** (`examples/simple_repeater/MyMesh.cpp:970-972`, `src/helpers/RoutingPolicy.h:15-21`).

So the only thing that can reach a phone twenty hops out is itself a flood, and the only thing that
must reach it is the *answer*: without a path, the phone cannot send a single session record.

The community firmware had already made this choice. Its reply routing policy names it as the case a
flooded request selects: `ReplyRoute::REPLY_ROUTE_PATH_RETURN` — "request arrived by flood: reply
with a PATH return, **flooded back**" (`src/helpers/RoutingPolicy.h:26-31`).

## Decision

**A route request is answered with a flooded reply**, `build_route_reply_flood`, laid out as:

```text
[0x01 flood][travel path byte][travel path]
[destination hash][sender hash]["LDRR"][responding radio's 32-byte key]
[the direct reply record: "LDRR", path byte, path, nonce]
```

- The reply is a `REQ` flood, so repeaters forward it up to 64 hops.
- It names the responding radio in full, so the phone can tell its own host's answer from any other
  node's, and it keeps the request's nonce so a second answer is a different packet rather than a
  duplicate the radio discards.
- The phone reads it out of its receive log (`PUSH_CODE_LOG_RX_DATA`, `0x88`), which is the same
  event that already teaches it a route from an overheard advert.
- The path inside is the route the host uses towards the phone, so the phone reverses it exactly as
  it reverses any other reply.

Three rules keep the extra air contained:

1. **Only while pathless.** Once a route is held, every session record is direct and nothing floods.
2. **Backing off.** An unanswered request doubles the wait from five seconds to a forty-second cap,
   with ±25 % jitter so several phones that started together do not ask together. Any route learned
   resets it.
3. **A route is only true while it works.** Both ends invalidate on the evidence: the phone drops the
   route when a session dies on it (`forgetRoute`), and the host treats an incoming request as proof
   that the path it holds is dead, because the peer asks only when nothing it sends is arriving.

## Consequences

- **First contact works at any distance the mesh reaches**, which is the whole point: 64 hops instead
  of 8, without a contact on either radio and without a firmware change.
- **Discovery costs one transmission per repeater in range**, not one in total. That is the price of
  an answer that arrives, it is paid only while the phone has no route, and it is the same price
  MeshCore's own path discovery pays.
- **A dead route is discarded within one ask instead of after `PEER_ROUTE_STALE` (120 s).** The old
  rule kept a *shorter* dead path in preference to a longer working one, which is exactly backwards
  when the shorter one has just lost a session.
- The direct reply is gone rather than kept alongside: at zero hops a flood is a single transmission
  too, so it covers the neighbour case at the same cost, and two reply paths would be two things to
  keep correct.

## Rejected

- **Flooding the session records themselves.** Impossible: a raw custom packet is only delivered on a
  direct route and is not forwarded. Reaching for a flood-routed payload type instead would mean
  every record costs one transmission per repeater in range.
- **An advert on demand as the answer.** Free on the phone's side (it already learns from adverts),
  but a repeater drops an advert after eight hops, and the requests observed here crossed fourteen.
  Kept as a bonus path, not the mechanism.
- **MeshCore's native path return (`PAYLOAD_TYPE_PATH`).** It requires the sender to be in the
  receiver's contact table so the payload can be decrypted, and ldmux keeps no contacts (ADR-0030).
- **A fixed retry interval.** Measured: one request every ten seconds, indefinitely, on a link that
  could not answer. Doubling with jitter is what a broadcast channel needs.
