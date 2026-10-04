# ldmux dev tasks — run inside `nix develop`.

# Build the Rust workspace (debug).
build:
    cargo build --workspace

# The gate: the whole Rust workspace.
test:
    cargo test --workspace

# The daemon's tests only.
daemon-test:
    cargo test -p ldmux-daemon

# Clippy, workspace-wide.
lint:
    cargo clippy --workspace

# Start the daemon in the foreground (default state dir).
daemon:
    cargo run -p ldmux-daemon --bin ldmuxd

# CLI against the running daemon, e.g. `just cli list`.
cli *args:
    cargo run -q -p ldmux-daemon --bin ldmux -- {{args}}

# Print remote connection info (host/port/token, and whether it is listening).
remote: (cli "remote")

# Pair the phone: a QR code to scan with the app (ADR-0017).
code: (cli "code")

# The same pairing, as a link.
pair: (cli "pair")

# Open a read-only mirror of a session on the desktop (i3 workspace 10).
#   just mirror <id>
mirror id: (cli "mirror" id)

# Build the native Android app (KMP + Compose) with the Gradle wrapper.
# Needs the `mobile` devShell (JDK 25 + Zig + NDK). Depends on the native core
# so the packaged .so always matches the current shim.
android: native-android
    nix develop .#mobile --command bash -c 'cd packages/mobile && ./gradlew :composeApp:assembleDebug'

# Cross-compile libghostty-vt + the JNI shim for Android (arm64-v8a, x86_64).
# Pinned Ghostty commit; see packages/mobile/scripts/build-libghostty-vt.sh.
native-android *abis:
    nix develop .#mobile --command packages/mobile/scripts/build-libghostty-vt.sh {{abis}}

# Build and install the debug APK on the attached phone.
android-install: android
    adb install -r packages/mobile/composeApp/build/outputs/apk/debug/composeApp-debug.apk

# The shim's own tests, on the host: no device, no daemon, no emulator. Covers the
# portable core (clipboard writes, the frame header contract, cursor movement).
native-test:
    nix develop .#mobile --command packages/mobile/scripts/test-native.sh

# The mobile tree's own TypeScript tests: the architecture guard, the frame
# header contract, and the native prebuilt's freshness. They need no device.
mobile-test:
    node --test packages/mobile/test/*.test.ts

# JVM unit tests for the shared logic (the frame decoder is the wire contract
# with the JNI shim, so it is pinned without needing a device).
android-test:
    nix develop .#mobile --command bash -c 'cd packages/mobile && ./gradlew :composeApp:testDebugUnitTest'

# Round-trip echo latency (the number predictive echo would improve).
# Over adb reverse this is loopback; point --url at the tailnet address for real.
latency sid *runs:
    nix develop --command node scripts/measure_latency.ts --session {{sid}} --runs {{runs}}

# Install the host side: the Rust binaries, the desk-window launcher, the systemd
# unit, and the nix GC root for the C library the binaries link against.
# Idempotent.
install: install-rust
    #!/usr/bin/env bash
    set -euo pipefail
    state="${XDG_STATE_HOME:-$HOME/.local/state}/ldmux"
    mkdir -p "$state" "$HOME/.local/bin" "$HOME/.config/systemd/user"
    chmod 700 "$state"
    # The GC root: the C library the daemon links against, so a
    # `nix-collect-garbage` cannot delete it out from under a running daemon.
    nix build .#runtime --out-link "$state/runtime"
    # The desk-window launcher ($mod+Return). It calls `ldmux window`, which is
    # the Rust CLI. There are no TS wrappers any more (D4: the TS tree is gone).
    sed "s|@REPO@|$PWD|g" "deploy/bin/ldmux-terminal" > "$HOME/.local/bin/.ldmux-terminal.new"
    chmod 755 "$HOME/.local/bin/.ldmux-terminal.new"
    mv "$HOME/.local/bin/.ldmux-terminal.new" "$HOME/.local/bin/ldmux-terminal"
    install -m 644 deploy/ldmux.service "$HOME/.config/systemd/user/ldmux.service"
    systemctl --user daemon-reload
    echo "installed ldmuxd, ldmux, ldmux-keeper and ldmux-terminal (tmux-free; D4 done)"
    echo "apply with: systemctl --user restart ldmux"

# Build the Rust workspace (release) and install the binaries. The daemon
# auto-spawns the keeper, so it must be on PATH beside the daemon.
install-rust:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release --workspace
    cp target/release/ldmuxd "$HOME/.local/bin/.ldmuxd.new"
    chmod 755 "$HOME/.local/bin/.ldmuxd.new"
    mv "$HOME/.local/bin/.ldmuxd.new" "$HOME/.local/bin/ldmuxd"
    cp target/release/ldmux "$HOME/.local/bin/.ldmux.new"
    chmod 755 "$HOME/.local/bin/.ldmux.new"
    mv "$HOME/.local/bin/.ldmux.new" "$HOME/.local/bin/ldmux"
    # The keeper binary (the daemon auto-spawns it; it must be on PATH).
    cp target/release/ldmux-keeper "$HOME/.local/bin/.ldmux-keeper.new"
    chmod 755 "$HOME/.local/bin/.ldmux-keeper.new"
    mv "$HOME/.local/bin/.ldmux-keeper.new" "$HOME/.local/bin/ldmux-keeper"
    echo "installed Rust daemon + CLI + keeper ($(cargo --version | awk '{print $2}'))"

# Keep both dev shells' toolchains as GC roots. Without this a `nix-collect-garbage`
# (run for any reason, by anything) deletes the JDK, NDK tooling and Zig the mobile
# build needs, and the next build re-downloads all of it.
pin-shells:
    #!/usr/bin/env bash
    set -euo pipefail
    state="${XDG_STATE_HOME:-$HOME/.local/state}/ldmux"
    mkdir -p "$state"
    nix develop .#default --profile "$state/devshell-default" --command true
    nix develop .#mobile --profile "$state/devshell-mobile" --command true
    echo "pinned: $state/devshell-{default,mobile}"

