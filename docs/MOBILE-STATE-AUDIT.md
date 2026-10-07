# Mobile session defects: audit and fixes

> Status: **fixed on branch `fix/mobile-session-defects`** (2026-10-06). The gate is
> green: `cargo test --workspace` (471 passed), `cargo clippy --workspace
> --all-targets -- -D warnings`, `cargo fmt --check`, the Kotlin unit tests
> (`scripts/android.sh test`), the native shim tests (`scripts/android.sh native-test`,
> 290 + 16 checks) and the mobile TypeScript suite (18 passed). §1.9 records
> follow-ups from an independent review of the first commit. Everything below names
> the code it is about, so a later reader can check it.

This is the record of the investigation that started with two reported symptoms:
after switching between open shells the terminal often could not be scrolled and
tap-to-cursor misbehaved, and shells "on desktop" did not open a window when
tapped on the phone. A wider state-lifecycle audit followed, because the same
class of fault (a composed object outliving the state it was built for) can hide
in more than one place.

## 1. What was wrong, and what fixed it

### 1.1 The root cause of the scroll and tap failures

**Symptom.** With several shells open, after switching between them, scroll,
tap-to-cursor and selection often did nothing on the shell now on screen. A
rotation or a re-open appeared to fix it.

**Cause.** `HorizontalPager` in `TerminalArea.kt` had no `key`, so its pages were
keyed by **index**. The session list is `attachable.filter { it.id in pageSelection }`
(`AppContent.kt`), and the daemon rebroadcasts it whenever anything changes, so the
shell at index N changes as the list reorders. Compose then reused the composition
at that index for a *different* shell. `rememberTerminalSurface` rebuilt the
controller for the new shell id (`TerminalSurface.android.kt:121`), and the old
holder was forgotten and disposed (`ControllerHolder.onForgotten`, `:904`). But all
five gesture blocks were `.pointerInput(Unit)` (`:379,433,562,586,696`), keyed on
`Unit` precisely so a gesture is not restarted mid-drag. A closure that is never
restarted keeps the value it captured, so the gestures went on addressing the
**disposed** controller of the shell that used to be at that index. A disposed
controller has a cancelled scope and a closed engine, so `scrollBy`, `tapCell` and
`selectionSet` were no-ops. `currentFrame`, `currentViewport`, `currentPainter` and
`currentPlacement` were already `rememberUpdatedState` (`:212-214,235`) for this
reason. `controller` was not.

**Fix.**
- Key the pager by the shell: `key = { page -> pages.getOrNull(page)?.id ?: page }`
  (`TerminalArea.kt:78`). A different shell is now a different composition, so the
  gestures are born with the controller they act on — and they survive a reorder
  rather than being rebuilt.
- Belt and braces: key each gesture on the controller itself
  (`.pointerInput(controller)`, `TerminalSurface.android.kt:379,433,562,586,696`) so
  a controller that changes for any other reason (a re-pair changing the connection
  key) restarts them too.

### 1.2 Effects that captured a stale controller

**Cause.** Three `LaunchedEffect`s called the controller but were not keyed on it, so
they kept the controller from the composition that launched them:

- `LaunchedEffect(active) { controller.setActive(active) }` (`:130`). Two shells can
  both be `active = true`, so switching from one to the other did not change the key
  and the new page's stream was never started.
- `LaunchedEffect(viewport) { controller.repaint() }` (`:268`).
- `LaunchedEffect(viewport, metrics...) { controller.setGrid(...) }` (`:272`). A
  page that adopted a new controller never told it the grid, so the pane could stay
  the old size.

**Fix.** All three now include `controller` in their key set.

### 1.3 A pinch could permanently disable taps

**Cause.** `detectPinch` set a `pinching` flag on zoom and cleared it in `onEnd`,
which ran only on the normal end path. A `pointerInput` restart (now a controller
change; before, a page-slot reuse) cancels the gesture, and the flag stayed set for
the life of the page. Every tap was then dropped as "probably a pinch" (`:370`).

**Fix.** The gesture body is wrapped in `try/finally`, so `onEnd` runs whenever the
gesture ends, cancellation included (`TerminalSurface.android.kt:970`).

### 1.4 History paging could wedge, and it could lose the page it just loaded

Three separate faults in `TerminalController`:

- **The latch stuck on a refused send.** `maybeExtendHistory` set
  `historyLoading = true` before `attach.scrollback(...)`, whose `Boolean` return
  was ignored. `sendControl` returns false when the socket is gone
  (`AttachClient.kt:528`), so one request issued during a reconnect disabled
  history for the life of the page.
- **The page-in reply could be applied in place and dropped.** `planSnapshot`
  checked the grid *before* the history flag, so a history reply that arrived after
  the pane was resized (another client, a rotation) took the in-place path. That
  path (`resizeReplayBytes`) re-establishes only the visible screen, so the history
  rows above it were discarded and the user was left at the top of the same shallow
  scrollback with no way to ask again.
- **Only one page was ever taken while anchored.** `historyRequested` reset only on
  an ordinary rebuild, so reaching the top again did nothing.

**Fix.**
- `SnapshotPlan.kt:59` checks `history` before the grid: a page-in always rebuilds.
- `maybeExtendHistory` (`TerminalController.kt:668-685`) leaves the latch clear when
  the send is refused, and maps the request to a growing page
  (`HISTORY_FIRST_LINES` 5000, doubling to the daemon's 20000 cap).
- On a page-in (`:308-338`) the engine's `scrollTotal` is compared with the value
  before the request: it grew, so the next ask doubles; it did not, so the keeper has
  no more and `historyExhausted` stops the asking. An ordinary rebuild resets the ask
  to the first page. This is the honest model: a page-in *replaces* the scrollback
  with the keeper's newest N rows (`serve_scrollback`, `remote.rs:1501`), so asking
  for the same N again would be a round trip for the same rows.

### 1.5 `repaint()` was dead

**Cause.** `TerminalHandle.repaint()` was an empty default (`TerminalHandle.kt:209`)
and `TerminalController` never overrode it, so the surface's first-layout
`controller.repaint()` (`TerminalSurface.android.kt:268`) did nothing. The case it is
for: a snapshot that arrived before the canvas had a size, rendered into nothing, with
no later event to re-render it.

**Fix.** `TerminalController.repaint()` (`:721`) calls `renderNow()`. The frame class
has no value equality, so this emits a fresh frame and the surface draws again.

### 1.6 A dead session id could keep a page alive

**Cause.** The selection-pruning effect removed ids that were *listed but not
attachable*. An id the daemon had stopped listing at all was never removed, so it could
keep the "none of my shells is left" rule (`chosenAlive`) from sending the app back to
the launcher.

**Fix.** `AppContent.kt:298-312` also removes ids absent from the list, guarded so it
only happens when the list is authoritative — connected, non-empty, and not waiting for
a shell that was just asked for (`pendingOpens == 0 && gotoSessionId == null`) — so a
reconnect's empty list cannot drop a live page.

### 1.7 "Send to desktop" did not offer the workspace you are on

**Symptom.** A shell opened on the phone says "send to desktop"; tapping it does
nothing that the user can see.

**Cause.** `DesktopPickerModal` filtered to workspaces that already had a window
(`windows.any { it.workspace == ws.name }`, old `Modal.kt`). The one place a person
wants a window is wherever they are looking, and on a fresh workspace that workspace is
empty — so it was not offered. With every workspace empty the dialog said "No desktop
has a window on it." and there was nothing to pick. The server path itself works: a
`socket makeLive`, a raw WebSocket `makelive`, and the phone's own pop-out → picker →
workspace all opened an `alacritty --class ldmux-live-<id>` window on this box
(2026-10-06).

**Fix.** `choosableWorkspaces` (`Modal.kt:231`) offers the occupied workspaces **and the
focused or visible one even when empty**; an empty workspace is labelled `(empty)`. The
empty-list message is now "No desktop is available."

### 1.8 A desk window could be left rendering a phone-sized pane

**Symptom.** The desk window for a session was 2552x2816 px but its pane was 48x17 —
"thin", every row the wrong width.

**Cause.** The pane is shared and its size is the last viewer's. Another device had
resized it to 48x17 (a phone viewport). The desk window's own tty was 158x82 and had
not changed, so it got no `SIGWINCH` and never sent a `resize`; it re-rendered every
snapshot at the wrong width for the rest of the session.

**Fix.** In the attach client (`crates/attach/src/lib.rs`), a `snapshot` whose grid
disagrees with this window's tty makes the window re-assert its own size
(`should_reassert`, `:185`; used at `:598`). It is bounded to once per mismatch so a
device that keeps resizing cannot turn it into an exchange, and a `--readonly` mirror
never resizes. Receiving a `snapshot` (rather than `owned`) means this window holds the
shell, so its size is the one that should win.

### 1.9 Follow-ups from an independent review

A read-only review of the first commit found four more things, all addressed:

- **The history latch was not cleared by an error reply.** `maybeExtendHistory` latches
  only after a successful send and only a `snapshot` cleared it. The daemon answers a
  failed scrollback with `scrollback_failed`/`not_attached` instead of a snapshot
  (`remote.rs:1467,1523`), and live output does not clear the latch either, so on an idle
  prompt history was disabled for the life of the page. `onServerError` now clears it
  (`TerminalController.kt:863-872`).
- **A `--readonly` mirror could resize the shared pane.** The SIGWINCH arm sent `resize`
  with no guard while the new re-assert path had one. It is now gated the same way
  (`crates/attach/src/lib.rs:755-768`).
- **The client engine's scrollback cap was unset.** The page-in growth test reads the
  *client* engine's `scrollTotal`, so it leaned on a library default that is not
  documented, and phone memory was bounded by that same default. It is now set explicitly
  to the keeper's bound, 10 000 lines / 40 MiB (`ldmux_terminal.h:113-121`,
  `ldmux_terminal.c:280-293`), and the shim was rebuilt for both ABIs.
- **A prune of ids absent from the session list was reverted.** It was added to stop a
  dead id accumulating, but an absent id cannot keep the app on a blank terminal
  (`chosenAlive` only looks at attachable shells), and the prune could drop a
  just-created shell whose list entry took longer than the 10 s `goto` timeout. The
  original "listed but not attachable" removal stands (`AppContent.kt:298-304`).

## 2. State-lifecycle audit: what was checked and found sound

- **Sockets.** `LdmuxClient` (list) and one `AttachClient` per page are stopped on the
  composition's dispose; `AttachClient.stop` cancels the job, closes the HTTP client and
  clears `ws`/`scope`. `wantTakeover`, `attempt` and `ended` are reset.
- **Controllers.** `ControllerHolder` disposes on forgotten *and* abandoned, and
  `dispose` is idempotent and cancels every job (`staleJob`, `latchJob`, `pendingTapJob`)
  and the scope.
- **The handle map.** `handles` is pruned in `DisposableEffect(handle, s.id)` with an
  identity check, so a re-registration cannot be dropped by the outgoing page.
- **Predictions and taps.** `pendingTapJob`/`pendingTap` are cleared on delivery,
  timeout and error; `mouseRefused` resets on every snapshot; `linkOffline`/`_rejected`
  reset on attach and round trip.
- **Pending shells.** `pendingOpens` is cleared on a create refusal, on disconnect, and
  when the daemon answers `created`; `gotoSessionId` is abandoned on timeout.
- **Audio.** `Dictation.stop()` on background and on a session switch.
- **Daemon.** `live_windows` is removed by `end_absent_windows`, `apply_reconcile` and
  `close_live`, and rebuilt on start (`rebuild_live_windows`, `daemon.rs:815`); the
  maintenance pass probes each window (`maintain`, `daemon.rs:934`) and only a definite
  `absent` ends a shell. `last_cwds` prunes ids that stop running. Claims persist with a
  60 s grace.
- **A correction worth recording.** An early reading of this audit concluded
  `reconcile_live` (`daemon.rs:707`) was never called in production. That was wrong: the
  live-window cleanup is wired through `maintain_plan` → `live_prepass` →
  `end_absent_windows` (`daemon.rs:950-990`), and `reconcile_live` is the test-facing
  wrapper. No bug there.

## 3. Remaining, deliberately not fixed here

- **One pane, two viewers.** The pane has a single size, so two clients of different
  screen sizes cannot both be right (ADR-0008). The re-assert (1.8) settles it in favour
  of the window that holds the shell, but the general answer — per-client viewport size,
  or a canonical grid each client reflows — is a design change, not a fix, and needs an
  ADR.
- **Switching away and back while following loses local scrollback.** Reactivating a page
  re-attaches and a snapshot while `viewportActive` rebuilds the engine for a clean
  parser state (`SnapshotPlan`). The keeper still holds the history and one reach to the
  top pages it back, but the client's own scrollback is not preserved. Preserving it is a
  decision about the attach protocol, not a bug.
- **The desktop inventory is a dependency.** "Send to desktop" needs the daemon's
  `desktop` reading. With no window manager adapter the window opens without being
  placed (`place_with`, `desktop.rs:1109`) and the phone shows the daemon's own `detail`.
