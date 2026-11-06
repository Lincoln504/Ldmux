<p align="center">
  <img src="docs/media/app-icon-pair.png" width="272" alt="the ldmux mark twice: `ld`, and the same mark flipped vertically, which reads `pl`">
</p>

# ldmux

A terminal that lives on a Linux desktop and reaches a phone: over the tailnet, or over a pair
of LoRa radios when there is no network at all.

## What it does

- **Shells run on the Linux desktop.** `ldmux-keeper` owns every PTY and supervises every shell,
  so restarting the daemon never ends one; replacing the keeper ends all of them and says so
  first.
- **The phone is a real terminal.** The app renders with libghostty-vt, the same engine the keeper
  runs, and parses the wire itself. No WebView, no browser, no remote desktop.
- **Two transports, one behaviour.** A WebSocket over the tailnet when there is a network, and a
  MeshCore radio link when there is not: identical panes, identical keys, identical ownership.
- **Sealed end to end.** Every record is a Noise session message inside a MeshCore `RAW_CUSTOM`
  packet, addressed to the peer radio's key; the radio's own channel encryption is never used.
  [`docs/SECURITY.md`](docs/SECURITY.md).
- **One owner per shell.** A second device is told who holds it and offered a takeover, never
  silently displaced.
- **LoRa is affordable.** Batch mode turns forty keystrokes into one transmission on `sync`, and
  the 1 % airtime allowance a European band requires is enforced on both ends.

## Components

| Where | What |
|---|---|
| Linux desktop | `ldmuxd` (daemon), `ldmux-keeper` (owning every PTY), `ldmux` (CLI), desk clients in an existing terminal (`ldmux attach`) |
| phone | the Android app, a KMP/Compose client with a native terminal renderer |
| radio route | a MeshCore radio on the host's serial port, driven by the daemon, and one the phone drives over Bluetooth |

## Modes

| Mode | Carried by | Notes |
|---|---|---|
| tailnet | a WebSocket to the daemon, bearer token, tailnet or loopback only | instant handshake, scrolling with momentum |
| LoRa live | two MeshCore radios | keystrokes leave as typed |
| LoRa batch | the same radios | typing leaves as one batch on `sync`, forced on a public channel |

<p align="center">
  <img src="docs/media/01-launcher.png" width="24%" alt="the launcher: new shell, the host's shells, and the menu on one this phone started">
  <img src="docs/media/02-shell.png" width="24%" alt="a shell in the app, the second of two">
  <img src="docs/media/03-batch.png" width="24%" alt="batch mode: 12 characters and one record held, waiting for sync">
  <img src="docs/media/04-landscape.png" width="24%" alt="landscape: the key panel beside the terminal">
</p>

## The app

- **First run** is an unpaired launcher: `scan a code`, `paste a link`, `use the radio`.
- **Launcher**: `new shell` first (asks for a path, empty means home), then the host's shells.
  Ticked rows become pages, numbered in tick order; `open` is the one action, dimmed while the
  host is unreachable.
- **Row menu**: a phone glyph marks a shell this phone started, `send to desktop` opens a
  workspace picker, `close shell` appears only for those, amber `no window` only when a window
  went away.
- **Between shells**: swipe the page or use the header arrows, which read the folder over
  `2 / 3 shells`. A shell another device holds reads `in use on <device>` with one `take it back`.
- **Keys, portrait**: `Esc` `Tab` `Home` `End`, `Ctrl` `Alt` `⇧` latches, `C-c`, `paste`, enter, an
  arrow pad with `PgUp` `PgDn`, a key that hides the keyboard, and the launcher.
- **Keys, landscape**: `paste` and a microphone, `Esc` `Tab` `del` enter, the same pad, `Home`
  `End`, and `C-c | - /`. Nothing latches; there is no soft keyboard.
- **Text size**: the `−` `+` stepper or a two-finger pinch; a bigger font reflows the shell.
  `controls` is compact or comfortable; `panel` is left or right and applies in landscape.
- **Scrolling**: drag and fling on the tailnet, or the `↑` `↓` `⇓` pad on both routes. A page is
  the pane less two rows; typing returns the view to live.
- **Selection**: long-press and drag with the platform's handles, releasing copies. A program's
  own copy is capped at 100 000 characters, stripped of control characters, and refused unless
  the host allows it.
- **Tap** moves the cursor: a click for a program that tracks the mouse, arrow keys for a prompt.
- **Dictation** types into the shell from the panel's microphone, prefers on-device recognition,
  and says when it falls back to a cloud recogniser. Portrait uses the keyboard's own.
- **Predictive echo** draws a keystroke before the host answers while the link is slow, turns
  itself off below the threshold, and never at a password prompt.
- **Leaving the screen** keeps the link for 45 seconds behind a notification, then detaches the
  pages and releases the claims while the shell keeps running.
- **Settings** has four sections: `link` (route `tailnet` or `LoRa radio`), `pairing` (token
  fingerprint, `pair` / `paste` / `unpair`), `diagnostics` (host, route, radio, build, and an
  in-memory log with `copy` and `show`), and `display`.
- **Radio setup** is in the app: pick the phone's radio over Bluetooth, choose a preset (region
  presets, the public table, `randomise`, `restore`, or a `custom` `MHz kHz SF CR`), `apply`.
  The desktop's radio row has one `match` that copies the four numbers and nothing else.
- **Input mode** is `live` or `batch`; `live` is refused on a public channel; the 36 second an
  hour limit is forced on between 869.4 and 869.65 MHz. The host applies the same allowance from
  the tuple the radio reports.
- **Batch bar**: what waits and what it costs (`12 characters, 1 record`), then `sending…`,
  `waiting for the answer` (until the host says the pane settled), or `no answer yet`, and
  `view N behind` when the screen has fallen behind. One button: `sync` while text waits, `show`
  when only screens do; held text is dropped when leaving the page.
- **Limits**: 64 live shells (`LDMUX_MAX_SESSIONS`), 10 000 lines or 40 MiB of scrollback each,
  both settable.
- **Android 9 or newer**, `arm64-v8a` or `x86_64`, and Tailscale signed in for the tailnet route.
  There is no iOS client: the radio route is the Android Bluetooth client.

`scripts/android.sh install` builds and installs over adb; there is no store listing.

## The host

Linux x86_64, a systemd user session, and nix for the binaries.

```bash
nix run .#install                 # binaries, desk-window launcher, both user units
systemctl --user enable --now ldmux
ldmux list                        # what the keeper has
ldmux shell ~                     # a shell in the current terminal
ldmux attach d8f92c0630           # an existing shell, in the current terminal
ldmux new ~                       # create one, print its id, open nothing
```

`ldmux` never opens a terminal window: it runs inside the one it was started from.

- The install leaves the keybinding to the operator: `ldmux-terminal` bound to a key opens the
  configured emulator and runs `ldmux shell` in it.
- The one window the daemon opens is the one the phone asks for (`send to desktop`), and it uses
  the terminal configured for that desktop: `ldmux terminal set alacritty` pins it, otherwise
  `$TERMINAL`, `xdg-terminal-exec` and the desktop's own setting decide. `ldmux terminal
  --explain` reports which, and where the answer came from.

`ldmux --help` lists every verb: `ping`, `list`, `new`, `shell`, `attach`, `terminal`, `send`,
`key`, `capture`, `kill`, `close`, `forget`, `adopt`, `mesh`, `code`, `pair`, `clipboard-write`.
`attach` takes `--readonly`, `capture` takes `--history`, `list` takes `--all` and `--json`,
`shell` takes `--name` and `--cwd`. `ldmux --version` and `ldmux ping` report the running build.
`nix run .#restart-keeper` ends every shell the keeper owns, after printing that it will.

## Pairing

Both routes pair by scanning a code from the host: from the unpaired launcher on a fresh install,
and afterwards from Settings then `pairing`.

```bash
ldmux code                        # tailnet: address and token, for a phone on the same tailnet
ldmux code --mesh                 # adds the radio's session key, secret and tuple
ldmux code --mesh-only            # radio keys only: no address, no token
```

`ldmux pair` prints the same as an `ldmux://` link, and a code can be pasted instead of scanned.
A `--mesh` code is as sensitive as the token and the secret together ([`docs/SECURITY.md`](docs/SECURITY.md)).

Both radios must share one frequency, bandwidth, spreading factor and coding rate. No MeshCore
channel key is involved: records are direct packets addressed to the peer's radio key and sealed
by the session, so a preset is a frequency to share, not a secret to match. The code carries the
host radio's tuple, so the app compares it with its own radio and matches it in one press. `ldmux
mesh status` prints the host's tuple; `ldmux mesh preset <MHz> <kHz> <SF> <CR>` sets it. The radio
link is one hop in one direction: a phone reaching the host through a relay attaches and never
hears a reply (`docs/ARCHITECTURE.md` §11).

## Build and test

```bash
nix develop                       # rustc/cargo, zbar for the QR tests
scripts/test-parallel.py          # the gate: the same binaries as cargo test, in a pool
cargo clippy --workspace --all-targets -- -D warnings

scripts/android.sh test           # the Kotlin JVM suite, including the wire fixture
scripts/android.sh build          # debug APK
scripts/android.sh install        # build, then adb install -r
scripts/android.sh release|bundle # signed APK / AAB, needs the signing inputs
```

Release rules, each of which fails the build rather than the upload:

- signing comes from `packages/mobile/keystore.properties` or `LDMUX_KEYSTORE*`
- `versionCode` comes from `git rev-list --count HEAD`, overridable with `LDMUX_VERSION_CODE`,
  and a release refuses to build without a real number
- R8 shrinks and obfuscates, with keep rules for JNI, manifest components and Compose members
- both native libraries must be present for every shipped ABI
- the licence texts must be collected, and the Kotlin suite must pass

The store material and the privacy policy are in `docs/RELEASE-AUDIT.md` appendix A.

The terminal engine is libghostty-vt, built from one pinned Ghostty commit by a nix derivation.
`nix develop` exports `LDMUX_VT_LIB_DIR`; outside the shell, point it at a directory holding
`libghostty-vt.a`. The Android SDK and NDK are host-provided (the mobile devShell finds
`$HOME/Android/Sdk`), so `scripts/android.sh test` stops at "SDK location not found" without them.

## Documents

- [`docs/SECURITY.md`](docs/SECURITY.md): what each route protects, what it does not, and where
  secrets live.
- [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md): layers, wire, invariants, security, failure
  modes, platform support, and how to bring up the two radios.
- [`docs/RELEASE-AUDIT.md`](docs/RELEASE-AUDIT.md): every way this fails, what is fixed, what is
  open, and the decisions still to make.
- [`docs/adr/`](docs/adr/): one record per decision, indexed in
  [`README.md`](docs/adr/README.md), including `0029-the-session-rides-raw-custom.md`.
- [`AGENTS.md`](AGENTS.md): for agents working in this repository.
