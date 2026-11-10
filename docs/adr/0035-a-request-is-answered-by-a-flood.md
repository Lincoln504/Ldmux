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

## Either end may ask, and both answer

A direct packet's path is consumed by the repeaters that carry it, so a session record arriving from
the phone teaches the host nothing about where the phone is. That is correct and unavoidable, and it
has a consequence the first version missed: **after a daemon restart the host holds no route while the
phone holds a fresh one**, and a phone holding a fresh route never asks. The phone would send `msg1`
into a host that could not address `msg2`, and nothing moved until the phone's handshake ladder gave
up and dropped its route — three unanswered handshakes and a minute of air, for a path the phone
already knew. The first version's own code called a request from the host "a dead letter".

So the mechanism is symmetric: **each end reads a request out of its receive log and answers it with a
flooded reply**, and the host asks while it has no route and traffic has moved either way within two
minutes. An idle pathless host stays quiet, because a host with no phone in range must not flood on a
timer.

## The answer goes back under the request's scope

A dense mesh configures its repeaters to deny unscoped floods (`region denyf *`), and some cap them
at a handful of hops (`flood.max.unscoped`). An unscoped answer to a scoped request is therefore
dropped before it crosses the region: the phone asks, the host answers, and nothing arrives.

So the reply is sent under **the scope the request arrived on**, which is MeshCore's own
`ReplyScope::REPLY_SCOPE_REQUEST`. The transport code is an HMAC over *the packet's own* payload and
type, so it is computed for the reply rather than copied from the request. This end can only honour
a scope it can name, so the order is: its own radio's scope when the request's code matches it, then
a public region whose derived key reproduces the code, then unscoped — which is exactly what an
unscoped request gets, and what a request from an unknown private region gets.

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
