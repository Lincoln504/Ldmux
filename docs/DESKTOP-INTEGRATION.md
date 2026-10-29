# Desktop integration: workspaces, windows and "make it live on the desktop"

> **Partly historical; current mechanism (2026-10-06).** The i3 findings below are
> measurements from 2026-09-15 and still hold for i3. The *mechanism* has changed twice:
> there is no shim script and no multiplexer client (a desk window is a terminal running
> `ldmuxd attach <id>`, a protocol client; `crates/attach`, ADR-0018), and the daemon no
> longer launches through `i3-msg exec`. It **spawns the terminal emulator directly**
> (`crates/daemon/src/desktop.rs`; detached, own process group, absolute paths, the
> desktop session's environment), and placing the window on a workspace is an **optional
> adapter** for i3, sway or hyprland that finds the window by its class and moves it
> (without one the window is "opened, not placed"). The terminal comes from
> `LDMUX_TERMINAL_CMD` (a command line with `{cmd}` and optionally `{class}`),
> `LDMUX_TERMINAL`, `$TERMINAL`, then a built-in autodetect list with a per-emulator
> template table. The window attaches over the daemon's local attach socket
> (`<control>-attach.sock`), so it needs no tailnet and no token. The daemon needs
> `DISPLAY` or `WAYLAND_DISPLAY` from its own environment or the user manager
> (`systemctl --user import-environment`); `ldmux mirror` uses the same argv with
> `--readonly`. That completion applies only to the daemon's desktop work (window
> launch, window-manager tools). **The shells** inherit the keeper's environment — the
> daemon's — so they lack the display variables, and display-dependent programs in an
> ldmux shell fail (open item in `docs/STATE.md`). Only the i3 adapter is measured; the
> sway and hyprland rows in the table
> below are still untested. The `i3-msg exec` findings below describe the old launch path
> and are kept as the record of what was measured.


The phone is not the only place an ldmux shell should be able to exist. The plan
of record is that a shell opened from the phone can be *made live on the desktop*
— a real terminal window attached to the same keeper session, on a workspace the
user picks — and that the launcher can show the workstation's workspaces, the
windows in them, and the pi sessions with their directories.

This file records what was **measured on the development box** (X11, i3 4.24,
alacritty 0.15.1) rather than reasoned about, because several natural
assumptions turn out to be false. Everything here was verified on 2026-09-15 and
the desktop was restored afterwards.

## The findings that change the design

**`for_window` is not an IPC command.** `i3-msg 'for_window [class="x"] move to
workspace 5'` fails to parse — the parser rejects `for_window` at the top of an
IPC command string. It exists only in the config file. The runtime equivalent is
`exec`, then a criteria-targeted move:

```
i3-msg 'exec -- alacritty --class ldmux-live-<id> -e <abs ldmuxd> attach <id>'
# poll `i3-msg -t get_tree` for window_properties.class == ldmux-live-<id> (~1-2 s)
i3-msg '[class="ldmux-live-<id>"] move to workspace number <N>'
```

**This is what keeps the user's desktop still.** `exec` alone does not move the
user's view, and a criteria-targeted `move` does not either: the probe window
landed on workspace 6 while the focused workspace stayed 10 throughout. The
old TypeScript `openMirror()` did the opposite — it ran `i3-msg 'workspace number N'`
*before* `exec`, which switches the user's visible workspace. That was the bug this
replaced; the Rust implementation is `crates/daemon/src/desktop.rs`.

**Do not quote a shell inside the i3 command string.** `exec -- alacritty --class
X -e sh -c "sleep 30"` silently produced no window at all: no error from
`i3-msg`, no process. Pass `-e <argv...>` with no internal quoting, or spawn from
the daemon's own process launcher (`crates/daemon/src/proc.rs`) and use `i3-msg exec` only for the
bare terminal invocation.

**`i3-msg -t get_tree` window nodes carry no `pid` on i3 4.24** — only `id`,
`window`, `window_properties{class,instance,title,role}` and `geometry`. To go
from a window to its process, use X11: `xprop -id <winid> _NET_WM_PID` (or
`xdotool getwindowpid <winid>`), which agree with each other.

**`xdotool windowactivate` switches the visible workspace.** Using it to give a
window focus moved the desktop from workspace 10 to 6. It is therefore unusable
for programmatic control here — and unnecessary: interact with the session
through the daemon (`ldmux send` / `ldmux key`), because the desktop window is only a viewer of it.

**`i3 --get-socketpath` must be called, not read from the environment.** `$I3SOCK`
is unset in a plain shell; the socket is
`/run/user/1000/i3/ipc-socket.<pid>`.

**`wmctrl` is not installed on this box.** `xdotool`, `xprop`, `dbus-send` and
`busctl` are. A pure-X11/EWMH fallback path (`_NET_WM_DESKTOP`,
`_NET_CURRENT_DESKTOP`) can be built on `xdotool` alone, but `wmctrl` cannot be
assumed present.

## A pi is linked to its session by its ancestry, not by its window

The desktop inventory is two views of the same machine that do not agree by
themselves. The window half sees an `alacritty` running `ldmuxd attach`; the pi
half sees a process in the keeper's pty. **The window is not in the pi's
ancestry** — the pi's parent chain goes pi → the pane's shell → `ldmux-keeper` →
`ldmuxd`, while the window hangs off the *daemon* as a separate child — so
`attribute_windows` finds nothing and a pi whose desk window is full of it
reports no window at all. Measured 2026-10-05 on five running pis: the four
inside desk windows reported no window, and the one running in a terminal the
daemon did not launch (an in-place `ldmux shell`, whose `alacritty` *is* an
ancestor) reported `ws 10`.

What is in the pi's ancestry is the session's **shell pid**, which the keeper
reports in every `State` (`pid`, the pane's process-group leader). The daemon
turns the keeper's list into `shell pid → session id` and walks each pi's parents
to it, filling `DesktopPiSession.ldmuxSessionId`. That was the TypeScript daemon's
mechanism too (tmux's `#{pane_pid}`; now ported to the Rust daemon), and its comment is why it matters: without it the phone could only guess
by directory, "and a guess that missed showed the user's running pi as *not in
ldmux* and opened a new shell instead".

`ldmux desktop` prints the link, and says **`window`** for a window whose
workspace the WM did not name — a present-but-empty `workspace` is not the same
fact as an absent one, and printing both as "no window" made a pi filling a
window read as the anomaly.

## Cross-desktop reality

There is no library that covers i3 + Sway + Hyprland + GNOME + KDE + XFCE.
PyWinCtl, CrossMacro and `ydotool` are the wrong tools to reach for: the first
two are input-automation (`/dev/uinput`) and do not enumerate or move windows at
all, and input injection is not needed here — the phone drives the keeper session,
never the desktop's input. So the abstraction is ours, as a per-backend adapter
with declared capabilities, so the UI can hide an action rather than lie about it:

| Backend | Enumerate workspaces/windows | Move window to workspace | Launch on chosen workspace |
| --- | --- | --- | --- |
| i3 | yes (`get_workspaces`, `get_tree`) | yes (criteria `move`) | yes (verified) |
| Sway | yes (i3 IPC compatible) | expected to port directly | expected; **not tested** (no Sway here) |
| Hyprland | yes (`hyprctl clients/workspaces/monitors`) | `dispatch movetoworkspacesilent` | expected; **not tested** |
| KDE/KWin | KWin scripting over D-Bus, or `kdotool` | possible via scripting | **not tested** |
| GNOME | needs a shell extension (Window Calls) or `Shell.Eval` | only via that extension | **not tested** |
| XFCE/other X11 | yes, EWMH (`_NET_CLIENT_LIST`, `_NET_WM_DESKTOP`) | `_NET_WM_DESKTOP` client message | launch then set `_NET_WM_DESKTOP` |
| Wayland generally | `wlr-foreign-toplevel-management` (wlroots only) | **no portable protocol** | no |
| GNOME/KDE on Wayland | not with any stable protocol | no | no |

`ext-workspace-v1` describes workspace *state*; it does not place windows. Where
a backend cannot do something, the launcher must say so rather than offering a
button that silently fails.

## Three things that make "make it live" silently do nothing

All three were measured on 2026-09-15 against a live i3, and all three present
identically: the call reports success and no window appears.

**1. `i3-msg` must be given the command as separate arguments.** This is the one
that cost the most time, because the wrong form is *accepted*:

| invocation | window? |
| --- | --- |
| `i3-msg "exec -- alacritty --class ldmux-live-x -e …"` | **no** — `[{"success":true}]`, nothing runs |
| `i3-msg exec -- "alacritty --class ldmux-live-x -e …"` | yes |
| `i3-msg exec "alacritty --class ldmux-live-x -e …"` | yes |

So the command is expanded into argv (`i3-msg exec -- <word> <word> …`), and each
word is still shell-quoted because `i3-msg` joins its arguments back into one
string that i3 runs through `sh -c`. `liveExecArgv()` holds this and is tested.

The lesson beyond the flag: **a command being accepted is not evidence it ran.**
i3-msg's success reply is a parse acknowledgement. What proved the window had
actually been created was polling `get_tree` for the class — which is why
`openLive()` reports "the window did not appear" instead of trusting the reply.

**2. The launched client must be an absolute path.** A live window is launched
*through the window manager*, which has the session's `PATH`, not the daemon's
(no `~/.local/bin`). A bare `ldmuxd` there is `not found` and the window closes
the instant it opens — it happened twice. `live_argv` therefore uses the running
daemon's own executable (`std::env::current_exe()`): the daemon *is* the attach
client, and a test asserts the path is absolute.

**3. A dead session has a keeper name, so it must be checked.** `requireTerminal()`
accepts any record with a non-empty `keeperName`, and a dead record keeps its name
until it is pruned. Attaching to it fails and the window closes the instant it
opens. `makeLive` refuses a session that is not `running`, with a reason.

## Session provenance

`SessionRecord.createdBy` exists and is set from the `createdBy` param of the daemon's
`new` handler (`crates/daemon/src/daemon.rs`), which hardcodes `"adopt"` for adoption. Live values on the dev
box are `pi-extension` (from before the extension was removed) and `local`.

**Correction to an earlier note here:** this did *not* need no schema change. It
was missing from `SessionInfo` (`crates/protocol/src/messages.rs`), which is
what the remote client actually receives — so the phone could not have filtered
on it even though the daemon knew. It is now part of `SessionInfo`, the remote
`new` path passes `createdBy: "phone"`, and the launcher shows the per-shell menu
only for shells that report it.

## Sessions must outlive the daemon — and do not, under `systemd-run`

Measured: stopping the test daemon's transient unit killed **every session**.
The sessions are not children of the daemon, but they are in its **cgroup**, and
a systemd unit's default `KillMode=control-group` sends the signal to everything
in the cgroup — including the keeper (the same held for the tmux server before
it), which is detached but cannot leave the cgroup it was started in.

This contradicts the stated design ("the daemon outlives every session"). The fix
is a property of the unit, not the code: `KillMode=process`, so stopping the
service stops the service and leaves the keeper (and every shell in it)
alone. `deploy/ldmux.service` carries it with the reason written next to it.
(Superseded in design by ADR-0021: the keeper is now its own unit and the shells are
scopes, so a daemon stop no longer reaches them at all; `KillMode=process` stays for the
non-systemd-keeper fallback.)

## Opening a shell in an arbitrary directory

`cwd` validation was, in the TypeScript daemon, only `existsSync && isDirectory`; the
Rust daemon resolves it through `crates/daemon/src/cwd.rs` (a leading `~` is expanded, an
absent directory means `$HOME`). The finding below is the original problem: a relative path silently resolves against the daemon's own working
directory. A path typed on the phone should be: absolute (or prefixed with
`$HOME` before it is sent), resolved with `realpathSync` so symlinks cannot
escape, then required to be the home directory or under it, and only then checked
for existence and directory-ness.
## A window that goes quiet (2026-10-25)

The desk window is `ldmuxd attach`, and it is the only client whose liveness depends
on itself: the daemon reaps a connection it has not *heard from*, and a window that
stops *hearing* has nothing watching it.

**What was found**, on the `lora` window, after the phone that had been using that
shell died: the pane was **46x22** (a phone's grid) inside a **158x40** window, the
window showed a frozen frame with no notice, the session had **no claim**
(`owner: null`), the client's connection to the daemon was ESTAB with empty queues,
and its process made **zero read syscalls in twenty seconds** while writing eight
bytes every four seconds. The shell was fine throughout (its `pi` finished its work;
the next snapshot showed it). Every other window held its shell; this one held
nothing and never learned the shell was free, because it never read the `sessions`
broadcast that said so. A `systemctl --user restart ldmux` revived it at its own
size, which is the proof that the pane and the daemon were healthy and the *client's
receive path* was not.

**Three holes, three fixes** (`crates/attach/src/lib.rs`):

- **Silence is now fatal within a bounded time** (`read_stall_limit`, seven ping
  intervals and never under 15 s). The keepalive is a `select!` branch, and that is
  its weakness: a loop stuck elsewhere never reaches it, so nothing is pinged, no
  pong is missed, and nothing reconnects. The receive wait itself is bounded now, so
  a link that delivers nothing ends the connection instead of the window.
- **Every write is bounded** (`SEND_TIMEOUT_MS`, 10 s). A `send` awaiting a flush
  that never completes holds the whole loop — no keys, no keepalive, no reconnect.
- **A refused size re-assert is retried** (`REASSERT_RETRY`, 2 s). The latch that
  stopped an endless exchange with a resizing device also made a *refused* re-assert
  permanent: the window sent its size while another device held the shell, the daemon
  refused it, and the latch stayed set — so when the phone left, the window kept
  painting the phone's grid.

**The rule, stated once**: a client's liveness must not depend on the loop that the
stall can block. The mobile client already does this (its pinger is a separate
coroutine that closes the socket after three missed pongs); the desk client now has
the equivalent as timeouts on its two waits. What is *not* bounded is the write to the
window's own pty: a terminal that stops taking output holds the client there. That is
not what this incident was (the terminal repainted as soon as a snapshot arrived), and
it is left as a known gap rather than guessed at.
