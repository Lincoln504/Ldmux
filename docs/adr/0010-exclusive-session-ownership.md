# ADR-0010: Exclusive session ownership, with consented handoff

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
- the desktop's live window shows *"open on another device — press Enter to take
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
