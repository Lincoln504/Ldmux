# ADR-0009: Node 26 + TypeScript, and removing the pi extension

- Status: accepted (2026-09-14)
- Supersedes: the Bun runtime choice implied by ADR-0001's implementation notes

## Context

The daemon, CLI, protocol and tests were built on Bun, and the project also
shipped a pi extension (`packages/extension`) that registered sessions with the
daemon. Two things changed:

1. **The extension is not part of the product.** ldmux is a remote terminal, not
   a pi plugin. The extension existed so a pi session would appear in the session
   list automatically; every other path (`new`, `adopt`, the mobile/web clients)
   already works without it.
2. **Bun was chosen, in part, for reasons that only applied to the extension** —
   chiefly running TypeScript directly, which made pi's loader story simple.

With the extension gone, the runtime is a free choice again, and the constraint
that remains is the useful one: the source must run without a build step, on a
mainstream runtime, with no bundler in the critical path.

## Decision

**Run on Node 26 with TypeScript stripped by Node itself. Remove the pi
extension and everything that existed only to serve it.**

- **Runtime: Node 26.8.2**, pinned in `flake.nix` from a second nixpkgs input
  (`nixpkgs-node`) because the main toolchain pin tops out at `nodejs_24`. Node 26
  is the Current line; the pin is by revision, not by tag.
- **No build step.** Node strips the types, so `node packages/daemon/src/main.ts`
  is the whole story. This costs one real constraint: **only erasable syntax**.
  `enum`, `namespace` and constructor parameter properties are gone from the
  codebase, and `erasableSyntaxOnly: true` in `tsconfig.json` makes the compiler
  enforce it so a future edit cannot reintroduce one and break the daemon at
  runtime only.
- **Tests: `node:test`**, with a small Jest-style `expect` in
  `packages/testing/src/expect.ts`. The suite had ~680 assertions written as
  `expect(x).toBe(y)`; rewriting them to `node:assert` would have been a large
  diff whose only risk was silently weakening an assertion. The *runner* is
  Node's; only the matcher vocabulary is local, and it is deliberately small
  enough to read in one sitting.
- **One production dependency: `ws`.** Node has a WebSocket *client* but no
  server, and the remote transport (`docs/REMOTE.md`) is a server. `ws` is the
  reference implementation; hand-rolling RFC 6455 would have traded a dependency
  for a protocol-implementation risk in the one place where a mistake is a
  security bug.
- **The extension is deleted**, along with `install.ts`/`pi-settings.ts`, the
  `install-extension`/`link-extension` CLI commands, and the `register` /
  `session_end` RPC methods and the RPC-only session TTL that only existed to
  expire records the extension would have stranded. `SessionKind` keeps its
  `"pi"` member: it is a session *label* that clients render, and someone can
  still run `pi` in an adopted tmux session.

## Consequences

- **Two nixpkgs inputs.** Necessary for "latest Node" without dragging the whole
  toolchain forward. If the main pin ever carries `nodejs_26`, delete the second.
- **Path resolution is now explicit.** Node's ESM resolver does not guess
  extensions, so every relative import carries `.ts`. This was already true, and
  it is why the port was small.
- **Three Bun-shaped assumptions had to go**, each of which was a latent bug:
  - A killed child could leave *its* children alive holding the pipes open. This
    was invisible under Bun only because Bun's test runner force-exits; the
    `control-plane` suite took 301 seconds and now takes 6. Children are spawned
    detached and killed **by process group** (`packages/daemon/src/proc.ts`).
  - Node's `WebSocket` hands binary messages back as a `Blob` unless
    `binaryType = "arraybuffer"`; every test client and the latency harness set it.
  - WebSocket control messages are now **ordered per connection** (chained like
    the Unix-socket dispatch). Two `attach` messages arriving in one tick used to
    race, so which snapshot a client received depended on transport timing.
- **`Bun.serve` → `node:http` + `ws`, and `RemoteServer.start()` is now async**:
  it resolves once the socket is listening, because with an ephemeral port (tests)
  that is the only way the caller learns the port.
- **The suite is much faster**: the full run is ~15 s where Bun's was ~60 s and a
  naive Node port was ~300 s.

## Alternatives considered

- **Stay on Bun.** No longer justified once the extension was gone; the only
  thing it bought was TypeScript execution, which Node now does.
- **Rewrite in Rust.** Considered and rejected: the daemon is I/O-bound
  orchestration around `tmux`, and the parts that would benefit from Rust (the
  terminal engine) are already native (`docs/adr/0007`). A rewrite would cost the
  whole protocol/client ecosystem and buy nothing measurable.
- **`node:assert` throughout.** Smaller dependency surface, but a ~680-assertion
  mechanical rewrite whose failure mode is a test that passes for the wrong
  reason.
- **Hand-written RFC 6455 server.** Rejected: a protocol implementation is exactly
  where an unnecessary risk should not be taken, and `ws` has no transitive deps.