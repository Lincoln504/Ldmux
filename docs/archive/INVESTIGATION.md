# ldmux — Investigation Log

> **ARCHIVE (moved to docs/archive/ 2026-10-06).** Historical record. Pre-port prior-art research from the tmux/TypeScript era; the current design is `docs/SPEC.md`.

> **Historical (2026-10-04, D4).** Written against the tmux implementation and left as
> the evidence base it was. Do not read it as current design: the session layer is
> the keeper (ADR-0018), and `docs/SPEC.md` is the authoritative design.


**Purpose:** Record what exists on this machine, what the prior art says, and the
evidence behind every load-bearing decision in [`SPEC.md`](../SPEC.md). This is the
"why" behind the spec.

**Method:** orient locally → parallel fast read-only forks over distinct areas →
live web research (knowledge store first) → verify key claims by hand → synthesize
spec. No source files were modified during investigation.

---

## 1. Machine facts (verified directly)

| Item | Value | Evidence |
|------|-------|----------|
| Host / tailnet name | `debian.tail496756.ts.net` | `tailscale whois 100.127.119.54` |
| Tailnet IP | `100.127.119.54` (`fd7a:115c:a1e0::5801:77cc`) | `tailscale ip -4` |
| Tailscale | **system `tailscaled`**, v1.102.3, running | `which tailscale`, `tailscale status` |
| Phone on tailnet | `motorola-edge-2024` (android, 100.120.129.96) | `tailscale status` |
| `tailscale serve` | *no config currently* | `tailscale serve status` |
| Window manager | **i3 4.24** on **X11** (`DISPLAY=:0.0`) | `i3-msg -t get_version`, `$XDG_SESSION_TYPE=x11` |
| i3 workspaces | `1..10`; `Mod+0 → workspace 10` | `~/.config/i3/config` lines 136–157, `i3-msg -t get_workspaces` |
| Terminal | **alacritty** (`~/.config/alacritty/alacritty.toml`) | config |
| i3 window helpers | `i3-msg`, `xdotool` present | `which` |
| tmux | **3.5a** at `/usr/bin/tmux`; **control mode `-C`/`-CC` supported** | `tmux -V`, `man tmux` |
| tmux server | none running on default socket; stale sockets exist (`dbx-drive*`, `default`, `tmux-1000/`) | `ls /tmp/tmux-1000/` |
| Node | v25.8.2 (nvm) | `node -v` |
| Node (devShell) | 26.8.2 | `node --version` |
| Rust | 1.95.0 / cargo 1.95.0 | `cargo -V` |
| Go | 1.24.4 | `go version` |
| gh | authed as **Lincoln504** (keyring), git user `Lincoln504` | `gh auth status`, `git config` |
| pi | **0.85.1** | `pi --version` |
| Pi RPC mode | **exists**: `pi --mode rpc` (args.js), `dist/modes/rpc/*`, `docs/rpc.md` | direct inspection |
| Reusable libs | `ws`, `better-sqlite3`, `typebox` in `~/.pi/agent/extensions/pi-research/node_modules` | `ls` |
| xterm.js reference | dooboxes desktop `src/components/Terminal.tsx` (xterm v5.5, fit+webgl); `node_modules` not installed | source |

> Note: one fast fork incorrectly concluded pi has no RPC mode (it grepped only
> `dist/cli/`). Direct inspection found `dist/modes/rpc/rpc-mode.js`,
> `rpc-client.js`, `jsonl.js`, `rpc-types.d.ts`, and `docs/rpc.md`, and
> `args.js` accepts `mode: text|json|rpc`. **Pi RPC is real.**

---

## 2. Existing projects on disk

### 2.1 gmux (the thing being replaced)

- `~/.local/bin/gmux`, `gmuxd` — stripped Go ELF, v2.1.0.
- Config: `~/.config/gmux/host.toml` (`[tailscale] enabled = false`).
- State: `~/.local/state/gmux/` — `state.db` (SQLite), `auth-token` (64-hex),
  `node-id`, `gmuxd.sock`, `sessions/<id>/{scrollback,scrollback.N}` (ANSI),
  `run/sessions/`, `tsnet/`.
- `gmux --help` surface: run (`gmux --`), `-d`, `agent prompt|logs|cancel`,
  `ls|attach|tail|send|wait|promote|reparent|kill|edit|open|remote|daemon|version`.
- Live sessions: `gmux ls` showed several `pi` sessions.
- Wrapped by `~/.bashrc` `pi()` function (`gmux -- pi "$@"`), with nesting guard
  (`$GMUX`), non-interactive bypass, and a stale-session warning after npm updates.

### 2.2 dooboxes (lessons source)

`~/Documents/dooboxes` — Rust daemon (`dooboxesd`) + session-bridge + CLI + Tauri
desktop; `alacritty_terminal` terminal core; proto/; extensive docs. Key docs read:
`TMUX_SESSION_MANAGEMENT.md` (800 lines), `TERMINAL_CODEC_DESIGN.md`,
`MOBILE_TERMINAL_UX.md`, `MOBILE_TERMINAL_PERFORMANCE.md`,
`ADR_2026-07-14_FLUTTER_MOBILE_AND_SEAMLESS_SESSIONS.md`,
`DECISION_2026-07_MOBILE_TAILNET_AND_NOISE_E2E.md`,
`docs/4_RESEARCH/current/mobile/MOBILE_TERMINAL_WEBSOCKET_RESEARCH_2026.md`.

Key takeaways:
- Session-bridge uses **`tmux -L <socket> -C attach-session`** (single `-C`),
  parses `%output`/`%begin`/`%end`, computes **capture-pane diffs**, uses off-lock
  async discipline to avoid blocking the runtime.
- Terminal codec: 16-byte framed header, message-type ranges, version negotiation,
  DoS limits.
- Mobile: chose **Flutter + flutter_rust_bridge + `alacritty_terminal`** with a
  CustomPainter; explicitly **rejected libghostty** (unstable ABI, no mobile
  target) and **xterm.dart**; "never split engines".
- Tailnet transport: TCP on the tailnet interface only; WireGuard is the E2E;
  Noise transport intentionally unwired. `tailscale serve` recommended for TLS.
- Mobile WebSocket constraints: iOS ~3 min background; Android Doze suspends;
  WiFi↔cellular causes 30–50% of disconnects; foreground service + wake lock.
- Testing lesson: tmux outlives the bridge by design; unique per-run socket dirs;
  e2e pins for every cross-component contract.
- Legacy Claude memory (`~/.claude/projects/-home-ldeen-Documents-dooboxes/memory/`)
  confirmed the libghostty rejection and the "never split engines" rule.

### 2.3 Other mobile/KMP projects

- `~/Documents/aura-keyboard`: Android Kotlin (not KMP); pure-Kotlin `:core` + IME.
- `~/Documents/mixar-app`: C++/Python (Blender fork), unrelated.
- `~/Documents/mobileappdev`, `~/AndroidStudioProjects/pa32`: classroom Android.
- **No KMP/Compose-Multiplatform or Flutter project exists on disk**; KMP is a
  greenfield decision, but the user's stated familiarity is KMP.

### 2.4 pi extension infrastructure

- Extensions load from `~/.pi/agent/extensions/*.ts|*/index.ts`, run **in-process**
  with jiti; lifecycle `session_start`/`session_shutdown`; `registerTool`,
  `registerCommand`, `ui.*`, `sessionManager`.
- Rule from pi docs: **do not start long-lived resources in the factory** — defer
  to `session_start`, clean up in `session_shutdown`.
- Reference implementations: `ollamainpi` (HTTP server started on `session_start`,
  idempotent guard, stopped on shutdown), `pi-research` (ships `ws`,
  `better-sqlite3`), `pi-fork` (child `pi` spawn/env overlay), `pi-blackhole`.
- pi sessions: JSONL at `~/.pi/agent/sessions/<encoded-cwd>/<ts>_<ulid>.jsonl`,
  header `{type:"session",version:3,id,timestamp,cwd}`, entries typed
  (`message`, `model_change`, `thinking_level_change`, `custom`, …). No lock/pid/
  heartbeat → liveness must be inferred (process scan and/or mtime).

---

## 3. Prior art (web)

### 3.1 gmux deep-dive

- Two Go binaries: per-session **runner** + per-machine **daemon**; embedded
  Preact/xterm.js web UI; SQLite state; on-disk rotating scrollback (1 MiB files).
- **Scroll bug + fix (the key lesson):** scroll-jump-to-top was pre-existing since
  v2.0.0, caused by stale `wasAtBottom`/`adjustedY` snapshots racing user wheel
  input across BSU→ESU→rAF windows (25 wheel-up notches → net -41 lines). Fixed in
  **PR #500** by replacing snapshot/restore with an explicit **`@gmux/scroll-anchor`
  mode state machine** (`following`/`anchored`); only user intent changes mode;
  output never moves the viewport. Related PRs #484/#502/#509/#514 fixed reconnect
  checkpoint fidelity and replay ordering. Accepted limits remain.
- Remote access via **embedded tsnet**, two-gate auth (Tailscale identity via
  WhoIs + bearer token), loopback bind by default, explicit peer model (no
  autodiscovery). Scrollback settings: `scrollback` default 5000, `smoothScrollDuration` 0.

### 3.2 tmux control mode & programmatic clients

- `-C` = control mode (canonical/echo on); `-CC` = no echo, DCS `\033P1000p`.
- Notifications: `%output %<pane> <octal-escaped>`, `%extended-output` (flow
  control), `%begin/%end/%error <ts> <seq> <flags>`, plus window/session/pane
  notifications.
- Output is **octal-escaped**; UTF-8 can split across notifications → per-pane
  `StringDecoder`. `%output` is not sent during `%pause`.
- Flow control: `refresh-client -f pause-after=N`, `%pause`/`%continue`;
  watermarks 512/8192 bytes. `refresh-client -C` lets a control client set size.
- `capture-pane -p -e -S -<n> -E -` for snapshots/scrollback; `-J` justify;
  history bounded by `history-limit` (default ~2000).
- Multi-client: unlimited clients per session share state; each client has its own
  viewport; `attach -d` detaches others; `attach -r` = read-only; grouped sessions
  via `new-session -t`.
- Existing libraries: **tmux-control-mode-js** (TS, protocol-only, no WS transport,
  tmux ≥3.2), **libtmux** (Python, shells out, not control mode), **node-tmux**
  (minimal), **tmuxy-core** (Rust `vt100` server-side emulation, adaptive
  throttling), **cdx** (`@xterm/headless` mirror server-side), **tmux-integrated**
  (VS Code TS parser with per-pane decoders), **webtmux** (Go, tmux message types).
- Recommendation from research: hybrid — control mode for streaming, optional
  server-side VT emulation for snapshots, xterm.js client.

### 3.3 Tailscale

- `tailscale serve` (tailnet-only, WSS/TLS, injects `Tailscale-User-Login` etc.;
  backend should be loopback) vs `tailscale funnel` (public, ports 443/8443/10000,
  bandwidth-limited — not needed).
- Identity: `tailscale whois` / LocalAPI `WhoIs` for per-request peer identity;
  tagged devices have tags instead of a user.
- Mobile: Android 8+, iOS app; OEM battery managers (Samsung/Xiaomi/OnePlus) can
  freeze the VPN; MagicDNS may fail in third-party apps → fall back to `100.x`.
- WebSocket over tailnet: heartbeat 15–30 s (mobile), 75% of shortest proxy
  timeout, application-level ping/pong (browsers can't send RFC6455 pings),
  exponential backoff with jitter, session-id resumption, immediate heartbeat on
  app foreground.

### 3.4 Mobile terminal clients (UX patterns)

- **Trustmux** (Byobu author): tmux + `tailscale serve` + PWA; reads tmux state
  via libtmux asynchronously; 6-digit pairing, 256-bit HttpOnly tokens, 90-day
  expiry, instant unpair.
- **ttyd/wetty/gotty**: WebSocket+PTY, xterm.js; tmux used for sharing; missed:
  no scroll anchor, weak mobile input; `-m 1` avoids cross-client resize contention.
- **mttyd**: ttyd mobile fixes — key bar w/ long-press secondaries, `inputmode=url`
  to defeat Gboard autocorrect, touch-drag-as-wheel for mouse-tracking TUIs,
  exponential-backoff reconnect.
- **Mosh**: transport-layer roaming + predictive echo; only syncs the visible
  screen (no scrollback) → pair with tmux.
- **Blink Shell / Termius / Termux**: extra-keys bar + sticky modifiers; gesture
  cursor movement with speed gears; foreground service + wake lock; tmux is the
  persistence layer.

### 3.5 pi clients (RPC bridges)

- `pi --mode rpc`: JSONL over stdin/stdout; `prompt`/`steer`/`follow_up`/`abort`/
  `new_session`/`get_state`/`get_entries`/`set_model`/…; responses `type:"response"`
  with `id`; rich event stream; no `readline` (U+2028/9). Also `RpcClient` for
  Node consumers.
- **pi-mobile** (Kotlin Compose, Android): Node bridge `127.0.0.1:8787` →
  `pi --mode rpc`; envelope with `channel:"bridge"|"rpc"`; token auth (constant
  time); per-cwd process isolation; control lock; reconnect grace 30 s; idle TTL
  5 min; session indexing from JSONL; cleartext scoped to `localhost` + `*.ts.net`.
- **pi-remote** (Bun bridge + PWA), **paseo** (multi-runtime daemon + E2E relay),
  **ygncode/pi-web** (Go, file-watcher + `tailscale serve`).
- **KMP networking**: Ktor `ktor-client-websockets` (OkHttp on Android, Darwin on
  iOS; the Android engine lacks WS); `ktor-reconnecting-websocket` gives backoff +
  `StateFlow` states; foreground service + `ConnectivityManager` for network-aware
  reconnect; `compose-webview-multiplatform` / `parkwoocheol/compose-webview` for
  WebView embedding.

### 3.6 Rendering engine landscape (2026)

- **xterm.js**: mature, 21k★, true color, mouse, IME, WebGL2 addon; the only
  production-grade mobile-capable terminal renderer.
- **BossTerm / Mosaic**: pure-Compose, **desktop/TTY only**, no mobile.
- **libghostty**: excellent on desktop but **no iOS/Android embedding target and
  unstable ABI** — not viable.
- **alacritty_terminal (Rust)**: proven core (dooboxes), but **no existing
  JNI/UniFFI bindings**; significant work; a future option.
- **Flutter + flutter_rust_bridge + alacritty_terminal**: dooboxes' path; viable
  fallback if KMP proves problematic.

---

## 4. Contradictions / corrections found during investigation

1. **pi RPC mode**: a fast fork said it does not exist; direct inspection disproves
   that. SPEC §4.5 relies on the correct finding.
2. **gmux tailscale enabled?** `host.toml` says `enabled=false`, but an embedded
   tsnet node `gmux-debian` exists and is offline. The *system* Tailscale is what is
   actually usable; the SPEC uses system Tailscale, not embedded tsnet.
3. **"workspace 0"**: i3 has no workspace 0; the user's `Mod+0` maps to workspace
   `10`. SPEC treats "last/0" as configurable, default `10`.
4. **KMP prior art**: no existing KMP project on disk despite the user's stated
   familiarity; KMP is greenfield here.

---

## 5. Transport deep-dive (v0.3)

Hands-on experiments on tmux **3.5a** + **Bun 1.3.9** (scratch server
`-L ldmux-probe`) established that **control mode `-C` runs over plain pipes with
no PTY**; only `-CC` needs a tty (`tcgetattr failed`). A Bun script drove
`tmux -C attach` over pipes, used `refresh-client -f pause-after=2`, and parsed
`%extended-output` correctly; a burst streamed ~383 KB in ~255 ms (~1.5 MB/s) at
~0 tmux CPU idle. `capture-pane -p -e` costs ~3.1 ms/call. This overturned the
v0.1/v0.2 assumption (inherited from dooboxes' "PTY-based" wording) that control
mode implies a native PTY dependency.

Source-level finding: dooboxes' `apps/session-bridge/src/tmux/control.rs`
control-mode client is **dead code** (no consumer); its shipped terminal path is
`capture-pane` diffs (`bridge/streaming.rs`), and its control parser has UTF-8
bugs. Copy `compute_diff` + the sanitizer policy; write a new parser.

Decision: ADR-0001 — control mode `-C` over pipes + `capture-pane`
snapshots/fallback, Bun + TypeScript, no native modules. Details in
[`TRANSPORT.md`](TRANSPORT.md).

## 6. Sources

Local: `~/.local/bin/{gmux,gmuxd}`, `~/.config/gmux/`, `~/.local/state/gmux/`,
`~/.bashrc`, `~/Documents/dooboxes/**`, `~/.pi/agent/**`,
`~/.config/nvm/.../@earendil-works/pi-coding-agent/**`, `~/.claude/projects/**/memory/**`,
`~/.config/i3/config`, `~/.config/alacritty/alacritty.toml`, `/tmp/tmux-1000/`.

Web (selected): gmux.app architecture/CLI/security/host-toml + GitHub PRs
#500/#484/#502/#509/#514; tmux wiki Control-Mode + man page; Tailscale
serve/funnel/MagicDNS/whois/LocalAPI + changelog; Mosh paper; trustmux/ttyd/wetty/
gotty/mttyd; Blink/Termius/Termux; pi-mobile/pi-remote/paseo/ygncode-pi-web; Ktor
WebSocket docs + `ktor-reconnecting-websocket`; xterm.js; BossTerm; alacritty_terminal;
`compose-webview-multiplatform`; SocketPipe; web-terminal-engine scroll-fidelity docs.