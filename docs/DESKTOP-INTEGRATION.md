# Desktop integration: workspaces, windows and "make it live on the desktop"

> **Partly historical (2026-10-04, D4).** The i3 rules, the workspace choice and the
> ownership handover are still the design. The *mechanism* is not: there is no
> shim script and no multiplexer client — a desk window is
> `<terminal> -e ldmuxd attach <id>`, a protocol client. See `crates/attach` and
> ADR-0018. `alacritty` below is the default, not a hardcoding: the terminal is
> `desktop::terminal()` (`LDMUX_TERMINAL`), and `ldmux mirror` uses the same argv
> with `--readonly`.


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
current `openMirror()` in `packages/daemon/src/desktop.ts` does the opposite — it
runs `i3-msg 'workspace number N'` *before* `exec`, which switches the user's
visible workspace. That is the bug this replaces.

**Do not quote a shell inside the i3 command string.** `exec -- alacritty --class
X -e sh -c "sleep 30"` silently produced no window at all: no error from
`i3-msg`, no process. Pass `-e <argv...>` with no internal quoting, or spawn from
the daemon's own process launcher (`proc.ts`) and use `i3-msg exec` only for the
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
to it, filling `DesktopPiSession.ldmuxSessionId`. That is the TypeScript's own
mechanism (tmux's `#{pane_pid}`, `origin/main:packages/daemon/src/daemon.ts:1096
-1106`), and its comment is why it matters: without it the phone could only guess
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

`SessionRecord.createdBy` exists and is set from the `NewParams.createdBy` param
in `handleNew()` (and hardcodes `"adopt"` in `adopt()`). Live values on the dev
box are `pi-extension` (from before the extension was removed) and `local`.

**Correction to an earlier note here:** this did *not* need no schema change. It
was missing from `SessionInfo` (`packages/protocol/src/messages.ts`), which is
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

## Opening a shell in an arbitrary directory

`cwd` validation today is only `existsSync && isDirectory` (`daemon.ts`), which
means a relative path silently resolves against the daemon's own working
directory. A path typed on the phone should be: absolute (or prefixed with
`$HOME` before it is sent), resolved with `realpathSync` so symlinks cannot
escape, then required to be the home directory or under it, and only then checked
for existence and directory-ness.