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
`libghostty` with little shipped. ldmux ships **xterm.js in a WebView behind a
`TerminalSurface` seam** — a seam that exists today only as the client's own
module boundary (`core.js` is DOM-free and unit-tested; `app.js` is the host
glue), with a native renderer as a later, optional swap behind it. Predictive echo and adaptive frame rate (mosh-derived, unimplemented
in dooboxes) are rejected until a measured problem exists.

### 5. No 16-byte magic/version/sequence frame (vs dooboxes)

ldmux keeps one **8-byte** header (type/flags/reserved/length). There is no magic
on a stream we own; the version is negotiated once in `caps`. Extra framing
fields and a second wire format are how dooboxes ended up with three
incompatible formats.

### 6. PWA is the v1 mobile client

The installable web client (landscape-locked manifest + service worker) is the
v1 Android experience. A native WebView shell is post-v1; nothing in
`packages/web` assumes a browser-only host.

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