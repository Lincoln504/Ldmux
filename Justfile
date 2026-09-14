# ldmux dev tasks — run inside `nix develop`.

# Unit + live tests (live tests spawn a scratch tmux server).
test:
    bun test

# Strict typecheck (no build step).
typecheck:
    bun run typecheck

# Start the daemon in the foreground (default state dir, tmux -L ldmux).
daemon:
    bun run packages/daemon/src/main.ts

# CLI against the running daemon, e.g. `just cli run shell`.
cli *args:
    bun run packages/daemon/src/cli.ts {{args}}

# Stream a tmux pane via control mode:
#   just probe <tmux-socket> <session>
probe socket session:
    bun run packages/daemon/src/probe.ts {{socket}} {{session}}

# Print remote connection info (host/port/token) + the tailscale serve command.
remote:
    bun run packages/daemon/src/cli.ts remote

# Pair the phone: https url + ldmux:// deep link + the tailscale serve command.
pair:
    bun run packages/daemon/src/cli.ts pair

# Expose the loopback remote over the tailnet via `tailscale serve`.
expose:
    bun run packages/daemon/src/cli.ts expose

# Open a read-only mirror of a session on the desktop (i3 workspace 10).
#   just mirror <id>
mirror id:
    bun run packages/daemon/src/cli.ts mirror {{id}}

# Symlink the pi extension into pi's auto-discovery dir, then `/reload` in pi.
# Enable the pi extension (writes an absolute path into pi's settings.json).
# A symlink into pi's discovery directory does NOT work: jiti resolves the
# extension's relative imports against the symlink path.
link-extension:
    bun run packages/daemon/src/cli.ts install-extension
# Reap tmux servers left behind by interrupted test runs or smoke scripts
# (never touches non-ldmux servers such as the user's own tmux sessions).
clean-tmux:
    #!/usr/bin/env bash
    set -euo pipefail
    dir="/tmp/tmux-$(id -u)"
    for sock in $(ls "$dir" 2>/dev/null | grep -E '^(ldmux|poll-debug)' || true); do
      tmux -L "$sock" kill-server 2>/dev/null || true
      rm -f "$dir/$sock"
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
