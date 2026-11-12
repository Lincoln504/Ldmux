# 0038 — Carrier selection: escalate on silence, decay on any arrival

**Status:** accepted (2026-11-10). Supersedes the carrier rule in ADR-0037, which said only *when*

> **Under revision, 2026-11-10/11.** The escalation rule stands, but the inbound flags that implement
> it conflate what arrived with what the next send should use, and "a direct record proves the direct
> path" is inbound proof only. `RADIO-PLAN.md` §4.2 keeps per-attempt choice and requires an
> outbound candidate to be proven by a correlated authenticated result instead. The capacity figure
> in *Consequences* below is corrected there too.
the two carriers exist; this says how one is chosen per send.

## Context

ADR-0037 gave a record two carriers: a `RAW_CUSTOM` direct packet along a known path, and a `REQ`
flood that every repeater forwards. A choice has to be made for every send, on both ends, and the
first two rules tried were both wrong in ways the field found within one trip:

- **"Flood when the route is not fresh"** made an idle-but-healthy link flood every keystroke.
- **"Reply on the carrier you were heard on"** is a positive feedback loop: each end hears a flood,
  floods back, and neither ever sends direct again, because the only thing that would clear the
  condition is a direct record arriving and no end ever sends one.

## Decision

Direct is the default. Flood is an escalation caused by a specific piece of evidence, and it decays
the moment that evidence changes.

| Evidence | What it proves | Effect on the next send |
|---|---|---|
| a **direct** record arrives from the peer | the path H→P carried *now* | path proven, failures = 0 → **direct** |
| a **flood** arrives | nothing about direct paths, but its travel chain reversed is a better candidate | adopt it (forcibly, see below), failures = 0 → **direct** |
| a direct send, then **silence** for one route round trip | the path P→H did not carry | failures = 1 → the next send **may flood** |
| **any** record arrives | something works | failures = 0 → **direct** |

Concretely:

- **One cheap attempt first.** A zero-hop direct packet is one transmission and is exactly right for
  a neighbour, so "no path at all" means "try the near field once", not "flood immediately". Only a
  direct attempt that produced nothing allows a flood.
- **The silence that counts is the path's own round trip** — six airtimes per hop, MeshCore's own
  direct-send factor — not the route's staleness bound. A minute of quiet is normal for a route; a
  minute of quiet after a send is a verdict.
- **A flood replaces the path forcibly**, even over one marked proven. The only reason the flood
  carrier is in use is that the path in hand did not carry, so the chain the flood actually crossed
  is the newest evidence there is. Without this the link cannot return to direct: the repair it just
  paid for would be refused as "longer than the one in hand", and the next send would flood for the
  same reason, for ever.
- **Floods are paced by a token bucket** (four back to back, refilled at one per flood round trip),
  because a batch is thirteen records and a terminal must not become a broadcast.

## Why this converges

- **Working link:** direct is answered inside its own round trip → failures stay 0 → direct for ever.
- **Path broken, mesh reachable:** direct → silence → flood → the flood's travel chain becomes the
  new candidate and is adopted forcibly → failures reset → direct on the new candidate. If that
  candidate is good, the link is direct again; if not, the cycle repeats and each flood supplies a
  fresh candidate. The steady state is one probe per repair, never a flood-only mode.
- **Nothing reachable:** floods repeat, bounded by the bucket and by the handshake budget, and the
  link fails honestly instead of spending the city's air for ever.

## Rejected

- **Mirroring the carrier** — positive feedback, above.
- **Flooding by staleness alone** — an idle link is not a broken one, and per-keystroke floods are a
  worse failure than the one they prevent.
- **Never flooding** — a `RAW_CUSTOM` record with no valid path reaches the operator's neighbours and
  stops, which is the state this whole design exists to escape.
- **Choosing a carrier by hop count** — the path's length says nothing about whether it carries;
  measured 2026-11-10, a 26-hop chain delivered while a 0-hop chain did not.

## Consequences

- The carrier is invisible above the link: the session sees one record either way, and the wire is
  identical. Only timing and capacity differ, and both follow the carrier that will actually be used.
- A record that will ride a flood is capped by the **receiver**, not the sender: the flood wrapper
  costs 12 bytes, the companion's receive-log header costs three more, and the flood's travel path
  grows at every repeater, so the usable record is `161 − path bytes` (`157` scoped). The 162 this
  record originally published was the send-side arithmetic alone and a full-size flooded record was
  therefore never reported to the receiving app. See `RADIO-PLAN.md` §4.1–§4.3 and
  `RADIO-AUDIT-REVIEW.md` §1–§3.
