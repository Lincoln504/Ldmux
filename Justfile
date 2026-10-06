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

# Clippy, workspace-wide, tests included, and a warning fails.
lint:
    cargo clippy --workspace --all-targets -- -D warnings

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

# Open a read-only mirror of a session on the desktop (workspace 10 where a window-manager adapter exists).
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

# Install the host side: the Rust binaries, the desk-window launcher and the
# systemd unit. Idempotent. Nix is optional: it only adds a GC root for the C
# library the binaries link against, and is skipped when `nix` is absent.
#
# An existing ~/.config/systemd/user/ldmux.service is never overwritten silently:
# the new unit is written beside it as ldmux.service.new, unless FORCE=1.
#   just install            FORCE=1 just install
install: install-rust
    #!/usr/bin/env bash
    set -euo pipefail
    state="${XDG_STATE_HOME:-$HOME/.local/state}/ldmux"
    bin="$HOME/.local/bin"
    unit_dir="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
    mkdir -p "$state" "$bin" "$unit_dir"
    chmod 700 "$state"
    # The GC root: the C library the daemon links against, so a
    # `nix-collect-garbage` cannot delete it out from under a running daemon.
    if command -v nix >/dev/null 2>&1; then
      nix build .#runtime --out-link "$state/runtime" || echo "note: could not pin the nix runtime (not fatal)"
    else
      echo "note: nix not found; skipping the nix GC root (only matters for nix-built binaries)"
    fi
    # The desk-window launcher (a keybinding runs it). It calls `ldmux window`.
    install -m 755 deploy/bin/ldmux-terminal "$bin/.ldmux-terminal.new"
    mv "$bin/.ldmux-terminal.new" "$bin/ldmux-terminal"
    # The unit.
    unit="$unit_dir/ldmux.service"
    if [ ! -e "$unit" ] || [ "${FORCE:-}" = 1 ]; then
      install -m 644 deploy/ldmux.service "$unit"
      echo "installed the unit: $unit"
    elif cmp -s deploy/ldmux.service "$unit"; then
      echo "the unit is already current: $unit"
    else
      install -m 644 deploy/ldmux.service "$unit.new"
      echo "NOTE: $unit exists and differs; the new one is $unit.new"
      echo "      review it (diff \"$unit\" \"$unit.new\"), then mv it into place, or rerun with FORCE=1"
    fi
    if command -v systemctl >/dev/null 2>&1; then
      systemctl --user daemon-reload || echo "note: systemctl --user daemon-reload failed (no user manager in this session?)"
    else
      echo "note: no systemctl; run ldmuxd yourself or use your init system"
    fi
    # Things that make the install work on this machine and are not ours to set.
    case ":$PATH:" in
      *":$bin:"*) ;;
      *) echo "WARNING: $bin is not on PATH; add it (e.g. in ~/.profile) or the CLI will not be found" ;;
    esac
    if command -v loginctl >/dev/null 2>&1; then
      linger="$(loginctl show-user "$(id -un)" -p Linger --value 2>/dev/null || true)"
      if [ "$linger" != yes ]; then
        echo "WARNING: linger is off; the user manager (and every shell) stops at last logout. Fix: loginctl enable-linger $(id -un)"
      fi
    else
      echo "note: no loginctl; make sure your user manager outlives logout"
    fi
    if [ ! -e "${XDG_CONFIG_HOME:-$HOME/.config}/ldmux/env" ]; then
      echo "note: no ~/.config/ldmux/env; without LDMUX_REMOTE_HOST the phone cannot connect (template: deploy/ldmux.env.example)"
    fi
    echo "installed ldmuxd, ldmux, ldmux-keeper and ldmux-terminal"
    echo "apply with: systemctl --user restart ldmux   (first time: systemctl --user enable --now ldmux)"

# Build the Rust workspace (release) and install the binaries. The daemon
# auto-spawns the keeper, so it must be on PATH beside the daemon.
install-rust:
    #!/usr/bin/env bash
    set -euo pipefail
    cargo build --release --workspace
    mkdir -p "$HOME/.local/bin"
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
    # **A running keeper is now OLD CODE, and nothing replaces it.**
    #
    # The keeper outlives the daemon on purpose (the ssh-agent pattern: restart
    # the daemon and your shells are still there), so `systemctl --user restart
    # ldmux` — the line this recipe prints — restarts *half* the system. The
    # running keeper keeps executing the binary it started from: measured
    # 2026-10-04, a keeper running since 13:39:30 served a keeper built at
    # 23:02:49, its `List` had no field the new daemon expected, and the daemon
    # silently fell back to a worse answer rather than failing. Every
    # keeper-side fix since the keeper started was deployed and not running.
    #
    # It cannot be replaced quietly: the keeper owns the PTYs, so replacing it
    # ends every shell (the documented single point of failure, plan R3). So say
    # so, loudly, and name the target that does it.
    live_keeper=""
    for c in /proc/[0-9]*/comm; do
      if [ "$(cat "$c" 2>/dev/null || true)" = ldmux-keeper ]; then
        live_keeper="${c#/proc/}"; live_keeper="${live_keeper%/comm}"; break
      fi
    done
    if [ -n "$live_keeper" ]; then
      started="$(ps -o lstart= -p "$live_keeper" 2>/dev/null | xargs || true)"
      echo
      echo "  WARNING: ldmux-keeper (pid $live_keeper) is STILL RUNNING THE OLD BINARY."
      echo "           started: $started"
      echo "           The keeper outlives \`systemctl --user restart ldmux\`, so the"
      echo "           daemon you are about to start will talk to the old keeper."
      echo "           Replacing it ENDS EVERY RUNNING SHELL. When that is what you"
      echo "           want:  just restart-keeper"
      echo
    fi

# Replace the running keeper with the installed binary.
#
# The keeper outlives the daemon, so nothing else replaces it and a deploy leaves
# it running old code (see `install-rust`). Killing it is the only way to load new
# keeper code, and it **ends every shell the keeper owns** — that is not a bug, it
# is the keeper's documented single point of failure (plan R3). The daemon notices the dead
# socket on its next tick and auto-spawns the new binary.
#
# The keeper is found by its process NAME (/proc/*/comm, exact match): matching a
# path on the command line can hit the daemon's own and kill the wrong thing
# (AGENTS.md). /proc, not pgrep, so it needs no procps.
restart-keeper:
    #!/usr/bin/env bash
    set -euo pipefail
    keeper_pid() {
      for c in /proc/[0-9]*/comm; do
        if [ "$(cat "$c" 2>/dev/null || true)" = ldmux-keeper ]; then
          p="${c#/proc/}"; echo "${p%/comm}"; return 0
        fi
      done
      return 0
    }
    pid="$(keeper_pid)"
    if [ -z "$pid" ]; then
      echo "no keeper is running (the daemon starts one on demand)"
      exit 0
    fi
    echo "ending the keeper (pid $pid) and every shell it owns"
    kill "$pid" 2>/dev/null || true
    tries=0
    while [ "$tries" -lt 40 ]; do
      sleep 0.25
      new="$(keeper_pid)"
      if [ -n "$new" ] && [ "$new" != "$pid" ]; then
        echo "keeper restarted: pid $new"
        exit 0
      fi
      tries=$((tries + 1))
    done
    echo "the keeper is gone but the daemon has not respawned one yet" >&2
    echo "check: journalctl --user -u ldmux -n 40" >&2
    exit 1

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

