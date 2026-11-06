# ADR-0027 — Credentials live outside the runtime directory

**Status:** Accepted (2026-11-06).

- **Date:** 2026-11-06
- **Related:** `deploy/ldmux.service`, `crates/protocol/src/paths.rs`,
  `crates/daemon/src/{auth,mesh,mesh_session}.rs`, ADR-0020 (the mesh session),
  `docs/ARCHITECTURE.md` §7

## Context

Under systemd the state directory is `%t/ldmux`, which is `$XDG_RUNTIME_DIR` and
therefore a tmpfs: it is empty after every boot. That is the right place for
sockets, the registry cache and ownership claims, which are either recreated or
only meaningful for the lifetime of a connection.

The bearer token used to live there. It was moved to `%S/ldmux/token` because a
token re-minted at boot locks out every paired phone (`crates/daemon/src/auth.rs`
says so in its own words). The radio route's credentials were left behind:

- `mesh-session.key` — the daemon's static X25519 key, which the phone pins at
  pairing;
- `mesh-pairing.json` — the pinned phone key and the replay timestamp floor;
- `mesh-psk` — the pairing's pre-shared secret.

After a reboot the daemon generated a new static key. The phone's pinned copy no
longer matched, the first handshake message failed as `Crypto`, the host sent no
reply, the phone spent its three attempts and landed on `SessionFailed`, and
nothing anywhere said the machine had rebooted. Recovering meant printing a new
code and scanning it, which is only discoverable if you already know the cause.

## Decision

1. **Two directories, one meaning each.** The state directory holds what is
   rebuildable or per connection. The **persist directory** holds credentials, and is
   `LDMUX_PERSIST_DIR`, else `$XDG_STATE_HOME/ldmux`, else `$HOME/.local/state/ldmux`,
   created 0700 and verified (`paths.rs`).
2. **Every credential moves to the persist directory**: the bearer token, the
   mesh static key, the pinned phone key and the PSK. The unit sets
   `LDMUX_PERSIST_DIR=%S/ldmux`, so the location does not depend on the process
   environment of the user manager.
3. **One adoption path, kept.** A daemon that finds those files in the old state
   directory moves them across once and says so in its log; a file already at the
   destination wins and the old copy is left in place, because a copy that came
   back is a fact worth seeing rather than deleting. The same rule already
   governed the token.
4. **A directory that cannot be made private refuses the start.** Group or other
   access, a foreign owner or a symlink at the path is an error naming the
   directory, because every file written there is a credential.

## Consequences

- A reboot no longer changes the daemon's mesh identity, so a paired phone keeps
  working across restarts, kernel upgrades and power cuts.
- `$XDG_STATE_HOME` is on persistent storage on every distribution this targets;
  a system that mounts it on tmpfs would reintroduce the defect, so the boot
  behaviour is a property of the host's filesystem, not of the daemon.
- The state directory keeps its single job, and nothing new is written into a
  tmpfs except things that are supposed to die there.
