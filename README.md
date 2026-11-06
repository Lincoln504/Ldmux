# ldmux

A terminal that lives on your own Linux machine and is reachable from your phone.
It works over your tailnet, or over a pair of LoRa radios when there is no network.

## What runs where

| Where | What |
|---|---|
| your Linux host | `ldmuxd` (the daemon), `ldmux-keeper` (owns the PTYs and the shells), `ldmux` (the CLI), desk windows (`ldmuxd attach`, or `ldmux window`) |
| your phone | the Android app: a native KMP/Compose client with its own VT emulator, no WebView |
| the radio, when there is one | a MeshCore radio on a serial port belongs to the host and the daemon drives it directly, and the phone drives its own radio over Bluetooth LE |

The keeper is the single source of truth. Every shell is a PTY owned by
`ldmux-keeper`, so a daemon restart does not end your shells, and the daemon talks to it
over a Unix socket. There is no tmux and no second session layer.

## The three modes

| Mode | Carried by | Notes |
|---|---|---|
| tailnet | a WebSocket to the daemon, bearer token, reachable only on your tailnet | instant handshake, the default |
| LoRa live | two MeshCore radios | keystrokes leave as you type |
| LoRa manual sync | the same radios | typing is held and sent on one action, which is forced on a public channel |

Only one phone talks to one desktop at a time. Whichever device holds a shell owns it,
and a second device that wants it is told so and offers to take it back.

## Getting it running on the host

Needs Linux x86_64, systemd (user units), and nix for the binaries.

```bash
nix run .#install                 # binaries, desk-window launcher, both user units
systemctl --user enable --now ldmux
ldmux list                        # what the keeper has
ldmux window ~                    # a shell in a desk window on the machine's own screen
```

`ldmux window` prints the session id. `ldmux --help` lists every verb, including
`new`, `send`, `key`, `capture`, `live`, `mirror`, `kill` and `close`.

Replacing the keeper ends every shell it holds. `nix run .#restart-keeper` says so and
asks first, and the daemon and the CLI restart without touching a shell.

## Pairing a phone

Both routes pair by scanning a code from the host, in the app under Settings then pairing.

```bash
ldmux code                        # tailnet: address and token, for a phone on the same tailnet
ldmux code --mesh                 # adds the mesh session key and the pairing secret
ldmux code --mesh-only            # mesh keys only: no address, no token
```

`ldmux pair` prints the same thing as an `ldmux://` link. A `--mesh` code is as sensitive
as the token and the secret together.

For the radio route both radios have to be on the same frequency, bandwidth, spreading
factor and coding rate, and the same channel key. The host prints its radio's operating
point when it starts, `ldmux mesh status` reports what it is doing now, and the phone's
radio panel can read a radio, adopt a public preset, or be given a tuple by hand.
`docs/MESH-HOST-RADIO.md` walks the host side.

## Build and test

```bash
nix develop                       # rustc/cargo, zbar for the QR tests
scripts/test-parallel.py          # the gate: the same binaries as cargo test, in a pool
cargo clippy --workspace --all-targets -- -D warnings

scripts/android.sh build          # debug APK
scripts/android.sh install        # build, then adb install -r
scripts/android.sh test           # the Kotlin JVM suite, including the wire fixture
scripts/android.sh release        # signed APK     (needs signing inputs)
scripts/android.sh bundle         # signed AAB     (needs signing inputs)
```

The Android release is signed from `packages/mobile/keystore.properties` or from
`LDMUX_KEYSTORE*` in the environment, and the tasks fail loudly without it.
`versionCode` comes from `git rev-list --count HEAD`, with `LDMUX_VERSION_CODE` and
`LDMUX_VERSION_NAME` to override it. A release refuses to package unless both native
libraries are present for every shipped ABI, and it carries the licence texts that
`scripts/collect-licenses.sh` collects. `docs/STORE.md` has the listing material.

The terminal engine is libghostty-vt, built from the single pinned Ghostty commit by a
nix derivation. `nix develop` exports `LDMUX_VT_LIB_DIR`, so plain `cargo` works inside
the shell. Outside it, point that variable at a directory holding `libghostty-vt.a`.

## Where to read more

Start with [`docs/STATE-OF-RECORD.md`](docs/STATE-OF-RECORD.md), which is the index: what
exists, what is deployed, what is open and what is known to be unfinished.

- [`docs/SPEC.md`](docs/SPEC.md) is the software specification.
- [`docs/PLATFORMS.md`](docs/PLATFORMS.md) says what is supported where, and which claims
  are verified.
- [`docs/MESH-STANDALONE.md`](docs/MESH-STANDALONE.md) is the radio route as built, with
  [`docs/MESH-HOST-RADIO.md`](docs/MESH-HOST-RADIO.md) for the host half.
- [`docs/RELEASE-AUDIT.md`](docs/RELEASE-AUDIT.md) is the release to-do, aspect by aspect.
- [`docs/adr/`](docs/adr/) holds the decisions and the reasoning behind them.
- [`docs/archive/`](docs/archive/) is history: the Rust port, the tmux study, the older
  state registers.
- [`AGENTS.md`](AGENTS.md) is for agents working in this repo.
