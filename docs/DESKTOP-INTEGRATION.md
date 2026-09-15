# Desktop integration: workspaces, windows and "make it live on the desktop"

The phone is not the only place an ldmux shell should be able to exist. The plan
of record is that a shell opened from the phone can be *made live on the desktop*
— a real terminal window attached to the same tmux session, on a workspace the
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
i3-msg 'exec -- alacritty --class ldmux-live-<id> -e tmux -L <sock> attach -t <session>'
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
through tmux (`send-keys`), because the desktop window is only a viewer of it.

**`i3 --get-socketpath` must be called, not read from the environment.** `$I3SOCK`
is unset in a plain shell; the socket is
`/run/user/1000/i3/ipc-socket.<pid>`.

**`wmctrl` is not installed on this box.** `xdotool`, `xprop`, `dbus-send` and
`busctl` are. A pure-X11/EWMH fallback path (`_NET_WM_DESKTOP`,
`_NET_CURRENT_DESKTOP`) can be built on `xdotool` alone, but `wmctrl` cannot be
assumed present.

## Cross-desktop reality

There is no library that covers i3 + Sway + Hyprland + GNOME + KDE + XFCE.
PyWinCtl, CrossMacro and `ydotool` are the wrong tools to reach for: the first
two are input-automation (`/dev/uinput`) and do not enumerate or move windows at
all, and input injection is not needed here — the phone drives the tmux session,
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

## Session provenance

`SessionRecord.createdBy` already exists (`packages/protocol/src/rpc.ts`), is set
from the `NewParams.createdBy` param in `handleNew()` and hardcodes `"adopt"` in
`adopt()`. So "only shells opened from the phone get a make-live/close menu"
needs **no schema change** — the mobile path passes `createdBy: "phone"` and the
launcher filters on it. Live values on the dev box today are `pi-extension` (from
before the extension was removed) and `local`.

## Opening a shell in an arbitrary directory

`cwd` validation today is only `existsSync && isDirectory` (`daemon.ts`), which
means a relative path silently resolves against the daemon's own working
directory. A path typed on the phone should be: absolute (or prefixed with
`$HOME` before it is sent), resolved with `realpathSync` so symlinks cannot
escape, then required to be the home directory or under it, and only then checked
for existence and directory-ness.