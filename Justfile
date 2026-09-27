# ldmux dev tasks — run inside `nix develop`.

# Unit + live tests (live tests spawn a scratch tmux server), plus the portable
# shim's own tests (which need a host libghostty-vt: built once, then seconds).
test: native-test
    node --test --test-concurrency=4 "packages/*/test/*.test.ts"

# Strict typecheck (no build step).
typecheck:
    node --run typecheck

# Start the daemon in the foreground (default state dir, tmux -L ldmux).
daemon:
    node packages/daemon/src/main.ts

# CLI against the running daemon, e.g. `just cli run shell`.
cli *args:
    node packages/daemon/src/cli.ts {{args}}

# Stream a tmux pane via control mode:
#   just probe <tmux-socket> <session>
probe socket session:
    node packages/daemon/src/probe.ts {{socket}} {{session}}

# Print remote connection info (host/port/token, and whether it is listening).
remote:
    node packages/daemon/src/cli.ts remote

# Pair the phone: a QR code to scan with the app (ADR-0017).
code:
    node packages/daemon/src/cli.ts code

# The same pairing, as a link.
pair:
    node packages/daemon/src/cli.ts pair

# Open a read-only mirror of a session on the desktop (i3 workspace 10).
#   just mirror <id>
mirror id:
    node packages/daemon/src/cli.ts mirror {{id}}

# Reap tmux servers left behind by interrupted test runs or smoke scripts
# (never touches non-ldmux servers such as the user's own tmux sessions).
clean-tmux:
    #!/usr/bin/env bash
    set -euo pipefail
    # tmux resolves a socket under ${TMUX_TMPDIR:-${TMPDIR:-/tmp}}/tmux-<uid>, and
    # `nix develop` sets TMPDIR to a nix-shell directory — which is where the suite's
    # sockets actually land, because `just test` runs inside it. This looked only at
    # /tmp, so it reaped nothing in the one environment the tests run in.
    for base in "${TMUX_TMPDIR:-}" "${TMPDIR:-}" "/tmp"; do
      [ -n "$base" ] || continue
      dir="$base/tmux-$(id -u)"
      # `ldmux-`, not `ldmux`: the bare prefix also matched the *production* socket,
      # `ldmux`, so this reaped every real session along with the test debris.
      for sock in $(ls "$dir" 2>/dev/null | grep -E '^(ldmux-|poll-debug)' | grep -vx -e ldmux -e "${LDMUX_TMUX_SOCK:-ldmux}" || true); do
        tmux -L "$sock" kill-server 2>/dev/null || true
        rm -f "$dir/$sock"
      done
    done
    echo "reaped stray ldmux tmux servers"

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

# JVM unit tests for the shared logic (the frame decoder is the wire contract
# with the JNI shim, so it is pinned without needing a device).
android-test:
    nix develop .#mobile --command bash -c 'cd packages/mobile && ./gradlew :composeApp:testDebugUnitTest'

# Round-trip echo latency (the number predictive echo would improve).
# Over adb reverse this is loopback; point --url at the tailnet address for real.
latency sid *runs:
    nix develop --command node scripts/measure_latency.ts --session {{sid}} --runs {{runs}}

# Install the host side: the flake's runtime (node + tmux, the versions the suite
# runs), the wrappers, and the systemd unit. The runtime link is a nix GC root, so
# garbage collection cannot remove what the daemon runs on. Idempotent.
install:
    #!/usr/bin/env bash
    set -euo pipefail
    state="${XDG_STATE_HOME:-$HOME/.local/state}/ldmux"
    mkdir -p "$state" "$HOME/.local/bin" "$HOME/.config/systemd/user"
    chmod 700 "$state"
    nix build .#runtime --out-link "$state/runtime"
    for f in ldmux ldmuxd ldmux-terminal; do
      sed "s|@REPO@|$PWD|g" "deploy/bin/$f" > "$HOME/.local/bin/.$f.new"
      chmod 755 "$HOME/.local/bin/.$f.new"
      mv "$HOME/.local/bin/.$f.new" "$HOME/.local/bin/$f"
    done
    install -m 644 deploy/ldmux.service "$HOME/.config/systemd/user/ldmux.service"
    systemctl --user daemon-reload
    echo "installed: $("$state/runtime/bin/node" --version), $("$state/runtime/bin/tmux" -V)"
    echo "apply with: systemctl --user restart ldmux"

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

