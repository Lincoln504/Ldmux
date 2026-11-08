# Decision records

One file per decision, oldest first. A record says what was decided, what it cost and what was rejected, because the reasoning is the part that does not survive a rewrite of the code. A superseded record stays in place: the history of a decision is part of it.

- `0001-transport.md` — Transport is tmux control mode `-C` over pipes (no PTY) + capture-pane snapshots. **Superseded.**
- `0002-canonical-geometry.md` — Canonical tmux geometry; clients never resize the pane. **Superseded.**
- `0003-session-lifecycle.md` — pi extension session lifecycle is reason-branched. **Superseded.**
- `0004-scroll-anchor.md` — The scroll anchor is client-owned.
- `0005-remote-tls.md` — Remote access via loopback bind + `tailscale serve`, token + QR pairing. **Superseded.**
- `0006-divergences.md` — Deliberate divergences from the earlier designs.
- `0007-native-terminal-engine.md` — Native terminal engine: libghostty-vt behind a shared KMP core.
- `0008-mobile-ui-modes-and-pane-ownership.md` — Mobile UI modes, and who owns the pane's geometry. **Superseded.**
- `0009-node-runtime-and-no-pi-extension.md` — Node 26 + TypeScript, and removing the pi extension. **Superseded.**
- `0010-exclusive-session-ownership.md` — Exclusive session ownership, with consented handoff.
- `0011-screen-orientation.md` — The device rotates, the keyboard does not.
- `0012-background-and-sleep.md` — A shell outlives the app, a claim does not.
- `0013-cross-platform-seams.md` — Cross-platform seams.
- `0014-text-selection-and-handles.md` — Text selection, and the two-point handles.
- `0015-session-layer-ownership.md` — The session layer: tmux keeps the PTYs. **Superseded.**
- `0016-direct-tailnet-bind.md` — The daemon listens on the tailnet address, and only there.
- `0017-qr-pairing.md` — Pairing is a QR code from `ldmux code`.
- `0018-session-layer.md` — The session layer is the keeper.
- `0019-wire-compression.md` — The remote WebSocket negotiates permessage-deflate.
- `0020-mesh-link-security.md` — Mesh link security: an end-to-end Noise session inside MeshCore. *(Accepted in practice since 2026-10-28; the status line still says proposed.)*
- `0021-keeper-as-unit-shells-as-scopes.md` — The keeper is a unit; every shell is a scope.
- `0022-terminal-queries-and-the-kitty-keyboard.md` — The terminal answers its own queries; modified Enter uses the Kitty keyboard protocol.
- `0023-session-crypto-dependencies.md` — The L3 session's crypto dependencies (`snow`, `hmac`, `sha2`, `zeroize`).
- `0024-voice-extension.md` — A pi extension returns, for voice only. *(Withdrawn 2026-11-05.)*
- `0025-mesh-terminal-profile.md` — The mesh terminal profile (plain-text row diffs inside the session).
- `0026-public-channel-mode.md` — Public-channel mode — typing held on the phone, one sync button.
- `0027-credentials-live-outside-the-runtime-dir.md` — Credentials live outside the runtime directory.
- `0028-the-desk-surface.md` — The desk surface: attach in your own terminal, and spawn only when ldmux must.
- `0029-the-session-rides-raw-custom.md` — Our session layer rides MeshCore's `RAW_CUSTOM` payload.
- `0031-a-keeper-restart-can-keep-the-shells.md` — A keeper restart can keep the shells: fd custody and a terminal snapshot. **Proposed.**
