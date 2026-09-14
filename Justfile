# ldmux dev tasks — run inside `nix develop`.

# Unit + live tests (live tests spawn a scratch tmux server).
test:
    bun test

# Stream a tmux pane via control mode:
#   just probe <tmux-socket> <session>
probe socket session:
    bun run packages/daemon/src/probe.ts {{socket}} {{session}}