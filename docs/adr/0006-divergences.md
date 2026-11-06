# ADR-0006: Deliberate divergences from the earlier designs

Status: Accepted (§2 superseded by [ADR-0018](0018-session-layer.md))

> Amended (2026-10-04, D4). The tmux session layer this ADR assumed is gone: `crates/keeper` owns the PTYs and runs one libghostty-vt per session, and the daemon is Rust. Kept for *why*, not as current design. See ADR-0018.

> §2 in particular is inverted. It said ldmux would not own a PTY layer and
> would keep tmux's grid. The port does own both: `crates/keeper` allocates the
> PTYs, and libghostty-vt holds the scrollback grid. The rest of the list still
> describes decisions that were considered and rejected, and remains useful.

## Context

Several designs in this space were considered before ldmux, and some of their
decisions were deliberately not adopted. Recording them prevents a future
contributor, or a review, from "fixing" ldmux back toward them.

## Decisions

### 1. No exclusive attachment lock

A design that models attachments explicitly takes an exclusive lock so two clients
cannot fight over the geometry. ldmux instead pins one canonical geometry
(ADR-0002) and lets any number of clients look at the same shell. A desktop
mirror attaches read-only, so there is no split-input risk. Ownership decides
who may write (ADR-0010), and the canonical geometry decides the shape. This is
simpler and preserves true view synchronization.

### 2. No separate scrollback store or session database

A PTY runner that persists per-session scrollback files plus a SQLite registry
with triggers and parent/child families was rejected. The keeper is the source of
truth: the PTY and the emulator's grid live in `crates/keeper`, and the daemon's
registry is a small JSON cache of metadata the keeper cannot hold. History reaches
a client on demand: a `scrollback` request is answered with a snapshot that
includes paged history, and the client's local buffer stays bounded at 1,000
lines on mobile.

### 3. No embedded second tailnet node

Embedding a tailnet node in the daemon was rejected because it needs an
interactive login or an auth key of its own. ldmux binds this machine's tailnet
address through the system `tailscaled` and answers only tailnet or loopback
peers (ADR-0016, superseding ADR-0005's loopback plus `tailscale serve`).

### 4. No renderer churn

A renderer direction that moved through three toolkits with little shipped was
rejected. ldmux ships one renderer, once: `packages/mobile` is a Kotlin
Multiplatform and Compose app drawing through libghostty-vt behind JNI
(`packages/mobile/native/`, ADR-0007). There is no WebView in it and no second
client to keep in step. Predictive echo and adaptive frame rate are adopted only
where a measured problem exists.

### 5. No 16-byte magic/version/sequence frame

ldmux keeps one 8-byte header (type/flags/reserved/length). There is no magic
on a stream we own, and the version is negotiated once in `caps`. Extra framing
fields and a second wire format are how a project ends up with three incompatible
formats to keep in step.

### 6. Native KMP/Compose is the v1 mobile client

An installable web client was built first and is not what ships. The v1 Android
client is the native Kotlin Multiplatform and Compose app in `packages/mobile`
(ADR-0007), the web tree was deleted rather than kept as a fallback, and a native
WebView shell was rejected: one client, shipped once.

### 7. There are no session kinds, and no nesting

Every session is a shell (2026-11-05). A design that records parent/child sessions
explicitly, with a parent id and a promotion path, was rejected: ldmux spawns
nothing by name, a shell is the only kind, and `pi` is a program a shell can run.
If nesting is ever needed, `SessionRecord` must gain a parent field and this ADR
must be revised. The divergence is intentional today, not an oversight.

## Consequences

- Reviews that expect a REST API, a SQLite store, an exclusive lock, or a second
  renderer will find none of them by design. The shipped surface is in
  `docs/ARCHITECTURE.md` §5.4, and the reasoning is here.
- Any change that adds a second session layer, a second wire format, or a second
  tailnet node requires a superseding ADR.
