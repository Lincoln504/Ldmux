# ADR-0006: Deliberate divergences from dooboxes and gmux

**Status:** Accepted

## Context

Two prior projects inform ldmux: `dooboxes` (Rust, container workspaces, mature
mobile-terminal specs) and `gmux` (a PTY-runner session manager whose scrolling
and mobile UX ldmux exists to replace). Several of their decisions were
considered and **deliberately not adopted**. Recording them prevents a future
contributor — or a review — from "fixing" ldmux back toward them.

## Decisions

### 1. No exclusive attachment lock (vs dooboxes)

dooboxes models attachments explicitly and takes an exclusive lock so clients
cannot fight over geometry. ldmux instead pins **one canonical geometry**
(`window-size manual`, ADR-0002) and lets any number of clients attach to the
same tmux session; a desktop **mirror** attaches read-only (`tmux attach -r`),
so there is no split-input risk. tmux is the only arbiter. This is simpler and
preserves true view synchronization.

### 2. No owned PTY, scrollback store, or session database (vs gmux)

gmux owns a PTY runner and persists per-session scrollback files plus a SQLite
registry with triggers and parent/child session families. ldmux owns **neither a
PTY nor scrollback nor session content**: tmux is the source of truth and the
registry is a small JSON cache of metadata tmux cannot hold. Persistence,
reconnect, and history are tmux's job.

Consequence: ldmux still owns no scrollback *store* — tmux keeps the history.
History reaches a client on demand: a `scrollback` request is answered with a
snapshot that includes up to 5,000 lines of paged history (`caps` advertises
`scrollback: true`), and the client's local buffer stays bounded at 1,000 lines.

### 3. No embedded second tailnet node (vs gmux `tsnet`)

gmux embeds a tailnet node, which requires an interactive login/`TS_AUTHKEY`.
ldmux binds to loopback and is exposed by the **system** `tailscale serve`
(ADR-0005), already proven on this machine.

### 4. No native renderer churn (vs dooboxes Flutter / libghostty)

dooboxes' renderer direction moved KMP → Flutter + `flutter_rust_bridge` →
`libghostty` with little shipped. This section used to say ldmux ships "xterm.js
in a WebView behind a `TerminalSurface` seam... with a native renderer as a
later, optional swap behind it" — that was true when written, but ldmux has
since shipped the native path itself: `packages/mobile` is a Kotlin
Multiplatform/Compose app rendering via libghostty-vt behind JNI
(`packages/mobile/native/`, ADR-0007), and `packages/web` (the xterm.js/WebView
client, `core.js`/`app.js`) was deleted entirely. The point being made against
dooboxes still holds — one renderer, shipped once, not three — it is just the
native one now, not xterm.js. Predictive echo and adaptive frame rate
(mosh-derived, unimplemented in dooboxes) are rejected until a measured
problem exists.

### 5. No 16-byte magic/version/sequence frame (vs dooboxes)

ldmux keeps one **8-byte** header (type/flags/reserved/length). There is no magic
on a stream we own; the version is negotiated once in `caps`. Extra framing
fields and a second wire format are how dooboxes ended up with three
incompatible formats.

### 6. Native KMP/Compose is the v1 mobile client

This section used to say the installable web client (landscape-locked manifest
+ service worker) "is the v1 Android experience" and that "a native WebView
shell is post-v1" — that has since been superseded: the shipped v1 Android
client is the native Kotlin Multiplatform/Compose app in `packages/mobile`
(ADR-0007), there is no WebView in it, and `packages/web` was deleted rather
than kept as a fallback. The PWA was real and shipped first; it is not what
ships now.

### 7. Child (fork/subagent) pi sessions are not ldmux sessions

Only **root** pi sessions register (`isChildAgentSession()` in the extension),
because fork children share the parent's workspace. gmux records children
explicitly (`launch_parent_id`, `promoted_to_root`) and can promote them. If
ldmux ever needs nesting, `SessionRecord` must gain a parent field and this ADR
must be revised — the divergence is intentional today, not an oversight.

## Consequences

- Reviews that expect a REST API, a SQLite store, an exclusive lock, or a native
  renderer will find none of them by design; see `docs/SPEC.md` §5.4 (marked
  aspirational) and this ADR.
- Any change that adds a second session layer, a second wire format, or a
  second tailnet node requires a superseding ADR.