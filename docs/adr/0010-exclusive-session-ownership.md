# ADR-0010: Exclusive session ownership, with consented handoff

> **Superseded (2026-10-04, D4).** The tmux session layer this ADR decided on is gone: `crates/keeper` owns the PTYs and runs one libghostty-vt per session, and the daemon is Rust. Kept for *why*, not as current design. See ADR-0018 and `docs/RUST-PORT-LOG.md`.

> The rule is unchanged and still implemented (`crates/daemon/src/ownership.rs`).
> One sentence in it is now false — the note at line 116 about the *tmux* pane
> size being the canonical one. The keeper is the canonical geometry now; the
> claim still hands the size over, which was the point.


**Status:** Accepted — **supersedes ADR-0006 §1**

## Context

ADR-0006 §1 rejected dooboxes' exclusive attachment lock in favour of allowing
any number of clients on one tmux session, with a read-only desktop mirror. That
decision was made when the desktop side was a *mirror* — a read-only second view
with no input of its own.

That is no longer the shape of the product. The desktop side is now a **live,
writable window** on a chosen workspace (`openLive`, ADR-0008-era feature), and
the phone is a **control-mode tmux client** on the same session. Two writable
clients is a different situation from one writable and one read-only: both can
type, both can resize, and neither knows the other is there.

Two concrete failures follow from that, and both have been observed:

1. **Geometry.** With `window-size manual` there is exactly one window size. Two
   clients of different sizes cannot both be *right*; ADR-0008 recorded this as an
   open problem with no solution inside a single shared window.
2. **Split input.** `tmux attach -r` was the protection against this and it only
   ever applied to the mirror, which no longer exists. A live window and a phone
   can now both type into one shell.

Nothing in the codebase can currently even *describe* the situation: there is no
device identity, no presence, no attach counting, no ownership. `attachable` and
`openedByPhone` are Kotlin-side derivations over wire data, so no server rule can
be built on them. `kill` has no ownership check at all while `makeLive` does.

Separately, dooboxes — the project this deliberately diverged from — specifies
the opposite, and specifies it as a *product rule* rather than a limitation:

> - Sessions allow only one attached device at a time (desktop local OR mobile remote)
> - **No automatic session stealing or forced detach without user consent**
> - Desktop has priority in simultaneous attach scenarios (desktop wins ties)

That spec was written for a world with a relay and a database. The behavioural
rule survives the tailnet pivot and is what this ADR adopts.

## Decision

**One device owns a session at a time.** Ownership is a fact the daemon holds,
tied to a live connection, and released the moment that connection ends.

### Ownership is a connection, not a record

The single most important lesson in dooboxes' own glossary is a failure mode it
names *Stale State*: persisted attachment state that outlives the connection
**blocks the next attach**. The failure is a lockout, not a cosmetic bug.

So ldmux stores ownership **nowhere**. It lives in memory, keyed by connection,
and a device's claims are dropped when its socket closes, when its session is
killed, and when reconcile finds the session gone. A daemon restart loses every
claim — which is correct, because a restart also loses every client.

### Consent is structural, so there is no tie to break

dooboxes needs a "desktop wins ties" rule because attachment there is an API call
that either side can make at any moment. Here, **both** directions require a
deliberate act:

- the phone shows a dialog — *"open on Desktop. Take it over?"* — and sends
  `takeover: true` only if the user confirms;
- the desktop's live window shows *"open on another device — press any key to take
  it back"*, and the keystroke is the consent.

Neither can steal silently, so a simultaneous race is not possible in a way that
needs a rule. Reclaiming **always** goes through the user, on both sides.

### The single client resolves the geometry question

ADR-0008's open problem — "two clients of different sizes cannot both be exactly
right" — stops existing rather than being solved. With one attached device there
is one size, and it is that device's. The viewing client keeps owning the
geometry (ADR-0008), and there is no second opinion to reconcile.

This is the reason to prefer exclusivity over a clever multi-client scheme:
the hard problem is removed instead of managed.

### Closing means "release my claim"

The two close gestures are not the same gesture, and previously they were one
code path (`closeLive`) with one meaning that was wrong for both:

| Gesture | Meaning |
|---|---|
| Close the desktop window | **End the session.** The window *is* the session, so closing it is the user disposing of it — not putting it away. It must never merely detach. |
| Close on the phone, session **live on the desktop** | **Release the phone's claim and nothing else.** The session stays, stays live on the desktop, and stays in the phone's list so it can be taken back later. |
| Close on the phone, session **not live on the desktop** | **End the session.** Nothing else is holding it. |

"Live on the desktop" is therefore a fact the daemon must hold, not a client-side
lookup: it is set when `makeLive` succeeds and cleared when the window is closed
or the session ends. Today the phone guesses this from a WM probe cached for one
second, which can be wrong in both directions.

## Consequences

- `SessionInfo` carries the owner and whether a live window exists, so the
  launcher can show *where a shell is open* from facts rather than a probe.
- A device announces itself once (`hello`), and the daemon keys ownership on the
  connection plus that identity. Without a device identity this ADR is
  unimplementable, so one is added to the protocol.
- The phone needs a stable per-install device id. It is not an account and not a
  security boundary — it is so two devices can be told apart in a message.
- A refused attach is a normal outcome, not an error: it is answered with who
  holds the session so the client can offer to take it over.
- The desktop's live window can no longer be a bare `tmux attach`, because it has
  to survive being detached (to show the message and offer the reclaim) instead
  of exiting. It becomes a small loop that re-attaches.
- ADR-0006 §1 is struck. Its reasoning was sound for a read-only mirror and does
  not survive the mirror becoming writable.
- ADR-0008's multi-client geometry section is no longer an open problem; it is
  now out of scope by construction. It should be read as history.
- A second session layer is still not introduced (ADR-0006 §2 holds): tmux stays
  the owner of session *content*, and this is a map of who is looking at it.

## Amendment (2026-09-18): the shell's origin decides what seizing disturbs

Exclusive ownership said a shell has one holder at a time. It did not say what
happens to the *desk's view* of a shell when the holder changes, and the first
answer — that a desktop window is detached whenever somebody seizes the shell,
whoever made it — is wrong in one direction and destructive in the other.

A shell has an **origin**, recorded when it is created (`createdBy`: `phone` for
anything asked for over the socket, `local` for the desk's own CLI, `adopt` for a
session ldmux found already running). The origin, and nothing about who is
seizing it now, decides:

- **A shell the phone started is the phone's.** A desktop window for it is the
  phone's way of being at a desk — that is what "send to desktop" makes — so when
  the shell goes back to the phone the window goes with it. There is nothing on
  the desk that wanted the window.
- **A shell the desk started is the desk's.** The phone borrows it: it may look
  and type, and the window it lives in is none of the phone's business. The desk
  keeps that window, and *using it again* is what takes the shell back — which
  the reconcile pass already detects, because the window re-attaches. Closing it
  on a seizure would destroy somebody's terminal on a fact they never agreed to.

Stated once as `seizingClosesTheDeskWindow(createdBy)` in `packages/daemon/src/daemon.ts`,
and tested as a rule rather than as a branch.

Three consequences worth recording, because each is a place this could go wrong
again:

- **`close` is not `kill`, and the *caller* decides, not the session.** A phone
  closing a shell it did not start is putting it down, not ending it. That check
  lives in the transport, not in the daemon's `close`: the same verb over the
  local socket is the desk's own CLI, where closing a shell it started must still
  mean ending it. Putting it in the daemon broke `ldmux close` the first time it
  was tried, and an existing test caught it.
- **Every shell is closed from the launcher and from nowhere else.** A shell is a
  thing chosen from a list, so ending one belongs in the list; the in-shell menu
  moved the most destructive action in the app one tap inside the screen you are
  typing into.
- **Going to the launcher detaches from every open shell.** The launcher is where
  you choose what to open, so arriving there lets go of everything. A page is a
  hold on a shell, and holding shells you are not looking at is how the phone
  keeps a session somebody else might want.

## Amendment (2026-09-26): a desk terminal is a desk window

The ownership model covered windows `makeLive` opened and nothing else, and the
terminal the desk actually uses — `$mod+Return` — was a bare `tmux attach` on a
shell nobody held. So for the most common desk shell none of this ADR applied: the
phone opened it without being asked, both sides typed into one pty, the phone's
claim resized the desk's window, and closing the window left the shell running.
(For four days after a reboot it was worse: the wrapper could not find `node`
under i3's PATH, and every window silently became a plain shell the phone could
not open at all.)

- **`ldmux window` / `run` / `shell` make the terminal a desk window.** The CLI
  creates (or, for `shell`, reuses) a shell, calls `deskWindow`, and the terminal
  execs the shim `makeLive` windows already run. The desk holds the shell from the
  start, so the phone gets `owned` and asks.
- **A seizure detaches a desk-started window; it does not leave it attached.** The
  2026-09-18 amendment said the desk *keeps* its window, and the code kept it
  attached — a second writer — and the reconcile pass read that client as the user
  taking the shell back and reclaimed it within a second. Detached, the shim shows
  "open on another device — press any key", and that key is the taking back.
- **The window is open exactly while its shim process is.** The shim records its
  pid beside its tty; the daemon asks the kernel, not the window manager, and so
  can tell a closed desk terminal (no window class of ours) from a detached one.
- **Closing the window ends the shell when the desk holds it.** When the phone has
  it, the window was only a notice, and closing it leaves the shell to the phone.
- **A page's claim goes with the page's socket.** Claims were released only when a
  device's *last* socket closed, and the launcher socket never closes, so every
  shell the phone had shown stayed claimed. Now a page socket releases the shell
  it was on unless another socket of the same device is on it (a reconnect).
- **A released shell's size goes back** to the terminals attached to it
  (`window-size latest`), rather than staying pinned at the phone's grid.
- **A restart rebuilds the window map** from the tty and pid each shim recorded.
- **A shell that exits ends its session** (`remain-on-exit off`), and a shell that
  ended by any route loses its claim, window record and shim.

Pinned by `packages/daemon/test/desk-window.test.ts`, each case checked to fail
with its fix removed.
