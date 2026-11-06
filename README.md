<p align="center">
  <img src="docs/media/app-icon.png" width="128" alt="the ldmux icon: a stroked l and d on near-black">
</p>

# ldmux

A terminal that lives on your own Linux machine, reachable from your phone: over your
tailnet, or over a pair of LoRa radios when there is no network at all.

| Where | What |
|---|---|
| your Linux host | `ldmuxd` (daemon), `ldmux-keeper` (owns every PTY and shell), `ldmux` (CLI), and desk clients in your own terminals (`ldmux attach`) |
| your phone | the Android app, which renders the terminal itself with no WebView |
| the radio route | a MeshCore radio on the host's serial port, driven by the daemon, and one the phone drives over Bluetooth |

The keeper is the source of truth. Every shell is a PTY it owns, so restarting the daemon
never ends a shell, while replacing the keeper ends all of them.

## The three modes

| Mode | Carried by | Notes |
|---|---|---|
| tailnet | a WebSocket to the daemon, bearer token, reachable only on your tailnet | instant handshake, the default |
| LoRa live | two MeshCore radios | keystrokes leave as you type |
| LoRa batch | the same radios | typing is collected and leaves as one batch on `sync`, forced on a public channel |

A shell belongs to whichever device holds it, so a second device is told who has it and
offered a takeover. The host accepts up to 64 connections. A shell started from the phone is
marked on its row, can be sent to a desk window, and is the only kind the phone can close.

<p align="center">
  <img src="docs/media/01-launcher.png" width="24%" alt="the launcher: new shell, the host's shells, and the menu on one this phone started">
  <img src="docs/media/02-shell.png" width="24%" alt="a shell in the app, the second of two">
  <img src="docs/media/03-batch.png" width="24%" alt="batch mode: 12 characters and one record held, waiting for sync">
  <img src="docs/media/04-landscape.png" width="24%" alt="landscape: the key panel beside the terminal">
</p>

Reach for the tailnet whenever there is a network: the handshake is instant, the terminal is
local, and scrolling has momentum. The radio is for when there is none, and there `batch` is
what keeps a long command affordable, since forty keystrokes become one transmission on
`sync` instead of forty, which is why a public channel forces it. On the radio there is no
swipe and no momentum, so the pad is the only way to move, and scrolling inside a full-screen
program spends airtime.

## On the phone

`scripts/android.sh install` builds the app and installs it over adb, since there is no store
listing. It needs Android 9 or newer, an `arm64-v8a` or `x86_64` phone, and Tailscale signed
in for the tailnet route. There is no iOS client: it is not in development, and it would be
tailnet only, because the radio route is the phone's Bluetooth client and that exists only
for Android.

- **First run** is an unpaired launcher that says what it needs and pairs from there:
  `scan a code`, `paste a link`, `use the radio`.
- **The launcher** is one list: `new shell` first, which asks for a path (empty means home),
  then the host's shells. Ticked rows become pages, numbered in the order you ticked them, and
  `open` is the screen's one action, dimmed while the host is unreachable.
- **A row's menu** says what the shell is and what may be done to it: a phone glyph marks one
  this phone started, `send to desktop` opens a workspace picker, `close shell` appears only
  for those, and amber `no window` only when a window went away.
- **Between shells** you swipe the page or press the arrows in the header, which reads the
  folder over `2 / 3 shells`. A shell another device holds reads `in use on <device>`, with one
  `take it back`.
- **Keys in portrait**: `Esc` `Tab` `Home` `End`, the `Ctrl` `Alt` `⇧` latches, `C-c`, `paste`,
  enter, an arrow pad with `PgUp` and `PgDn`, a key that hides the keyboard, and the launcher.
- **Keys in landscape**: `paste` and a microphone, `Esc` `Tab` `del` enter, the same pad,
  `Home` `End`, and `C-c | - /`. Nothing latches, because there is no soft keyboard.
- **Text size** is the `−` `+` stepper or a two-finger pinch, and a bigger font reflows the
  shell. `controls` is compact or comfortable, and `panel` is left or right, which only shows
  in landscape.
- **Scrolling** is drag and fling on the tailnet, or the `↑` `↓` `⇓` pad, which is always
  there. A page is the pane less two rows, and typing returns the view to live.
- **Selection** is a long-press and a drag with the platform's own handles, and releasing
  copies. A program's own copy is capped at 100 000 characters and stripped of control
  characters, and the host refuses it unless you allow it: `ldmux clipboard-write on`, off by
  default.
- **A tap moves the cursor**, as a click for a program that tracks the mouse and as arrow keys
  for a plain prompt.
- **Dictation** types into the shell from the panel's microphone, prefers on-device
  recognition, and says on screen when it falls back to a cloud recogniser. Portrait uses the
  keyboard's own.
- **Predictive echo** draws a keystroke before the host answers while the link is slow, turns
  itself off below the threshold, and never at a password prompt.
- **Leaving the screen** keeps the link for 45 seconds behind a notification, then detaches
  the pages and releases the claims, while the shell keeps running in the keeper.
- **Settings** has four sections: `link` (route `tailnet` or `LoRa radio`), `pairing` (with a
  fingerprint of the token and `pair` / `paste` / `unpair`), `diagnostics` (host, route, radio,
  build, and an in-memory log with `copy` and `show`), and `display`.
- **The radio** is set up in the app: pick the phone's radio over Bluetooth, then choose a
  preset (region presets, the public table, `randomise`, `restore`, or a `custom` `MHz kHz SF CR`) and
  `apply`. `desktop's radio` has one `match` that copies the four numbers and nothing else.
- **Input mode** is `live` or `batch`, `live` is refused on a public channel, and the 36 second
  an hour airtime limit is forced on between 869.4 and 869.65 MHz. The host applies the same
  allowance from the tuple the radio reports, and the line under the switch is the radio's own
  spent airtime.
- **The batch bar** says what waits and what it costs (`12 characters, 1 record`), then
  `sending…`, `waiting for the answer` (until the host says the pane has settled, so it stops when
  the answer stops), or `no answer yet`, and `view N behind` when the screen has fallen behind. Its one button is `sync` while text waits and `show` when only screens do,
  and held text is dropped when you leave the page.
- **Limits**: 64 live shells (`LDMUX_MAX_SESSIONS`) and 10 000 lines or 40 MiB of scrollback
  each, both settable.
- **Privacy**: nothing is collected, there is no analytics or crash SDK, and the token is
  sealed under a non-exportable Keystore key and excluded from backup.

## Security

- **The tailnet route** is a bearer token in an `Authorization` header, checked at the upgrade,
  and the daemon listens only on your tailnet or on loopback. There is no public listener and no
  way to turn authentication off.
- **The radio route is sealed end to end.** A record is a `Noise_IKpsk2` message
  (`Noise_IKpsk2_25519_ChaChaPoly_SHA256`) inside a MeshCore `RAW_CUSTOM` packet addressed to the
  peer radio's key, so MeshCore's own encryption is never applied and no channel key is involved.
  The preset the two radios share is a frequency, not a secret.
- **Why not use MeshCore's channel encryption**: its channel envelope is AES-128-ECB with no
  nonce, a two-byte tag and one shared key per channel, which is deterministic and forgeable at
  1 in 65,536 per attempt, and per-contact traffic uses a static secret that is never rekeyed.
  `docs/adr/0029-the-session-rides-raw-custom.md` has the evidence and the alternatives.
- **What the session gives**: a nonce that never repeats, a full 16-byte tag, separate keys per
  direction, a replay window, forward secrecy per session, and a fresh handshake every 65,536
  records or 12 hours instead of a rekey that reuses the nonce.
- **What it does not give**: no ratchet, so no post-compromise security; record sizes follow the
  text; and the radio's path, node hashes and traffic volume are plaintext by MeshCore's design,
  so an observer sees that two radios are talking even when not what they say.
- **The pairing code is the secret.** A `--mesh` code carries the session secret and the bearer
  token together, so treat a photo of it as a copy of both. Re-pair to rotate.
- **On disk**: the bearer token, the mesh static key, the pinned phone key and the secret are
  0600 files in the persist directory (`~/.local/state/ldmux`), never in the tmpfs runtime
  directory. On the phone they are AES-256-GCM under a non-exportable Keystore key and excluded
  from backup.
- **Clipboard writes from the host are refused by default**; `ldmux clipboard-write on` is an
  explicit grant, and the app shows what a program tried to copy.
- Nothing is collected: no analytics, no crash SDK, no telemetry.

## Getting it running on the host

Linux x86_64, a systemd user session, and nix for the binaries.

```bash
nix run .#install                 # binaries, desk-window launcher, both user units
systemctl --user enable --now ldmux
ldmux list                        # what the keeper has
ldmux shell ~                     # a shell in *this* terminal
ldmux attach d8f92c0630           # an existing shell, in this terminal
ldmux new ~                       # create one and print its id, opening nothing
```

`ldmux` never opens a terminal window of its own: it runs inside the one you have, and the
install leaves the keybinding to you, so `ldmux-terminal` bound to a key opens your configured
emulator and runs `ldmux shell` in it. The one window the daemon opens is the one the phone
asks for (`send to desktop`), and it uses the terminal you configured: `ldmux terminal set
alacritty` pins it, otherwise `$TERMINAL`, `xdg-terminal-exec` and your desktop's own setting
decide. `ldmux terminal --explain` says which, and where the answer came from.

`ldmux --help` lists every verb: `ping`, `list`, `new`, `shell`, `attach`, `terminal`, `send`,
`key`, `capture`, `kill`, `close`, `forget`, `adopt`, `mesh`, `code`, `pair` and
`clipboard-write`. Of those, `attach` takes `--readonly`, `capture` takes `--history`, `list`
takes `--all` and `--json`, and `shell` takes `--name` and `--cwd`. `ldmux --version` and
`ldmux ping` say which build is running, and `ldmux terminal --explain` says which emulator a
phone-requested window would use and why. `nix run .#restart-keeper` ends every shell the keeper
owns, and prints that before it does it.

## Pairing a phone

Both routes pair by scanning a code from the host. A fresh install pairs from the unpaired
launcher, and after that it is Settings then `pairing`.

```bash
ldmux code                        # tailnet: address and token, for a phone on the same tailnet
ldmux code --mesh                 # adds the radio's session key, secret and tuple
ldmux code --mesh-only            # radio keys only: no address, no token
```

`ldmux pair` prints the same as an `ldmux://` link, and a code can be pasted if the phone
cannot scan. A `--mesh` code is as sensitive as the token and the secret together.

For the radio route the two radios must be on the same frequency, bandwidth, spreading
factor and coding rate. No MeshCore channel key is involved: records are direct packets
addressed to the peer's radio key and sealed by the session, so the channel preset is a
frequency to share, not a secret to match. The code carries the host radio's tuple, so the
app can compare it with its own radio and match it in one press. `ldmux mesh status` prints
the tuple the host is on and `ldmux mesh preset <MHz> <kHz> <SF> <CR>` sets it. The radio
link is one hop in one direction: a phone reaching the host through a relay attaches and
never hears a reply (`docs/ARCHITECTURE.md` section 11).

## Build and test

```bash
nix develop                       # rustc/cargo, zbar for the QR tests
scripts/test-parallel.py          # the gate: the same binaries as cargo test, in a pool
cargo clippy --workspace --all-targets -- -D warnings

scripts/android.sh test           # the Kotlin JVM suite, including the wire fixture
scripts/android.sh build          # debug APK
scripts/android.sh install        # build, then adb install -r
scripts/android.sh release|bundle # signed APK / AAB (needs the signing inputs)
```

A release is signed from `packages/mobile/keystore.properties` or `LDMUX_KEYSTORE*` in the
environment and fails loudly without it. `versionCode` is `git rev-list --count HEAD`
(`LDMUX_VERSION_CODE` overrides it), the bundle refuses to package unless both native
libraries are present for every shipped ABI, and it carries the licence texts
`scripts/collect-licenses.sh` collects. The store material and the privacy policy are in
`docs/RELEASE-AUDIT.md` appendix A.

The terminal engine is libghostty-vt, built from one pinned Ghostty commit by a nix
derivation. `nix develop` exports `LDMUX_VT_LIB_DIR`; outside the shell, point it at a
directory holding `libghostty-vt.a`. The Android SDK and NDK are **host-provided** (the mobile
devShell finds `$HOME/Android/Sdk`), so `scripts/android.sh test` on a machine without one
stops at "SDK location not found" — install the SDK and NDK first.

## Where to read more

- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): layers, wire, invariants, security,
  failure modes, platform support, and how to bring up the host radio.
- [`docs/RELEASE-AUDIT.md`](docs/RELEASE-AUDIT.md): every way this can fail, what is fixed,
  what is open, and the decisions still to make.
- [`docs/adr/`](docs/adr/): the decisions and the reasoning behind them, including
  `0029-the-session-rides-raw-custom.md` for why the radio route carries its own session.
- [`AGENTS.md`](AGENTS.md): for agents working in this repo.
