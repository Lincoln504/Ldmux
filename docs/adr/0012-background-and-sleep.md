# ADR-0012: A shell outlives the app, a claim does not

**Status:** accepted (2026-09-18)

## Context

The app keeps a shell open while it is off screen. It must: a remote terminal
that died the moment you answered a message would be useless. But "keep it open"
was implemented as *keep everything* — sockets, a foreground service, a partial
wake lock and a claim on the shell — for as long as the app was installed and a
shell was open.

The cost of that was never stated, so it was never weighed:

- One socket per open shell, each pinging every 4 s. Three shells is ~45 radio
  wakeups a minute, indefinitely.
- A `PARTIAL_WAKE_LOCK` held from `onStartCommand` to `onDestroy`, which prevents
  the CPU from suspending. A phone left with a shell open did not sleep.
- The claim, which is a statement that *somebody is using this shell*. Held by a
  phone in a pocket, it is a lock with no reader: it blocks the desk from opening
  the shell with no takeover.

Meanwhile the thing that actually matters — the shell — never needed any of it.
It lives in tmux on the host. Nothing the client does can lose it, and a client
that reconnects is given a full snapshot of the terminal *state* by construction.

## Decision

**The app is on screen or it is not, and being off screen ends in letting go.**

- `AppLifecycle` states the fact (set from `onStart`/`onStop`, not `onPause`: a
  dialog and a keyboard are not the user leaving).
- Off screen for `BACKGROUND_GRACE_MS` (45 s) and the app drops its pages, which
  stops the service, releases the wake lock, closes the sockets and releases the
  claims. Coming back inside the window changes nothing.
- Letting go is the **same mechanism** as the launcher button — dropping the pages
  *is* the detach — so there is one way for a shell to be let go of, not two.
- Returning lands on the launcher, which is honest: the app is a list of shells
  until you open one.

## Consequences

- **The grace window is what makes it not annoying.** An app switch, a glance at a
  notification, a reply to a message all come back inside it and find everything
  exactly as it was.
- **The battery claim is now true.** The wake lock is released when the app leaves
  the screen, which is what the service's own doc comment had claimed all along.
- **Nothing is lost by sleeping**, because the resync is already the correctness
  backstop: `attach` carries the grid, the daemon resizes *before* snapshotting,
  and the client rebuilds its engine from the snapshot.
- **A claim no longer outlives the connection that made it, and now it does not
  outlive the *last* one either.** A device holds one socket for the session list
  and one per open shell, and a reconnect overlaps the new socket with the old —
  so releasing on any close let a dead socket free the claim its own replacement
  had just made. The shell went quietly claimable by anyone, with no takeover and
  no message. Fixed, and the regression test fails without the fix.
- **Not yet done:** `AttachClient` retries every 1.5 s with no cap and no network
  wake, while `LdmuxClient` uses bounded exponential backoff plus a
  validated-network wake. Two retry policies for one app is drift waiting to
  happen, and the attach client is the one that runs per page.
