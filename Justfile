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
link-extension:
    ln -sfn {{justfile_directory()}}/packages/extension ~/.pi/agent/extensions/ldmux
    @echo "linked: ~/.pi/agent/extensions/ldmux -> packages/extension"