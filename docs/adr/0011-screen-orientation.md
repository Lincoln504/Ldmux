# ADR-0011: The device rotates, the keyboard does not

**Status:** Accepted, amended 2026-09-27 (the launcher holds portrait; see the end); supersedes ADR-0008 §1

**Supersedes:** [ADR-0008](0008-mobile-ui-modes-and-pane-ownership.md) §1 — the
locked-orientation decision.

## Context

The app used to own the screen's orientation. A flip button locked the activity to
portrait or landscape, and the keyboard was welded to that: portrait *meant* the
keyboard was up, and putting the keyboard away was implemented as the platform
telling us the IME inset had gone to zero, which we answered by rotating.

That coupling made both halves worse.

- **The keyboard could not be dismissed without moving the screen.** Closing it and
  changing the shape were one action, so the user could have both or neither.
- **The orientation disagreed with the device.** A phone half-tilted on a desk, a
  foldable, a tablet in a stand, split screen — the app had one opinion and the
  hardware had another, and the app's won.
- **The lock had to be written from exactly one place to be sane.** It was, but the
  rule only existed because two components would otherwise have fought over one
  activity's orientation.
- **The IME watcher that detected a dismissal could not tell why the keyboard went
  away.** Our own focusable popups (the menu, the settings dialog) hide the IME too,
  so opening the menu read as "the user dismissed the keyboard" and flipped the
  screen 600 ms later. Distinguishing the two needed a debounce, and the debounce was
  a workaround for a rule that should not have existed.

## Decision

**The device owns the orientation. The app owns the keyboard. They are not the same
decision and they no longer share a variable.**

- The manifest declares no `screenOrientation`, and declares `configChanges` for
  orientation, screen size, density, keyboard and uiMode, so the activity is not
  recreated when the device turns. `termux-app` does exactly this, and it is the
  precedent for a terminal app.
- The shape is read, not decided: `LocalWindowInfo.current.containerSize` compared on
  its two axes. That answers auto-rotate-off, a foldable's inner screen, split screen
  and a floating window correctly and without a second opinion. `LocalConfiguration`
  and sensor readings were both rejected as instruments — one reports the display
  rather than the window, the other reports the hardware rather than the pixels we
  are about to draw into.
- **The keyboard exists only in portrait.** In landscape there is no text field, so
  `typing` is false there by construction rather than by a rule someone has to
  remember.
- **Closing the keyboard does not rotate anything.** A control in the portrait bar
  clears the keyboard request and nothing else; the screen stays exactly where it is.
  Rotating out to landscape and back re-arms it, which is the only way back and is
  the platform's own gesture for "show the keyboard" on a device with no keyboard.
- **Opening a shell does not rotate the device either.** It used to force landscape,
  which is the app asserting an opinion about hardware again.
- The IME watcher is deleted. Nothing needs to know *why* the keyboard closed.

## Consequences

- No flip control exists anywhere: not in the menu, not in the portrait bar, not in
  the landscape panel. The landscape panel's bottom row is now the microphone alone.
- `Orientation.kt` and its Android actual were deleted here. They came back with the
  amendment below, as a policy seam (`ScreenOrientation`) rather than a lock button.
- Auto-rotate-off now means the app is stuck in one shape, which is what the user
  asked for by turning auto-rotate off.
- A user who closes the keyboard in portrait gets a portrait terminal with no
  keyboard. That is a legitimate state, and it is the one the user asked for: the bar
  still sends keys, so the shell is still usable.

## Amendment (2026-09-27): the launcher holds portrait

Asked for by name: the launcher (the list and every dialog opened from it: new shell,
settings, the desktop picker) stays portrait and does not auto-rotate. The shell view
is unchanged and follows the device.

- **Nothing here is the keyboard coupling this ADR removed.** No state is shared
  between orientation and the keyboard, and closing the keyboard still rotates
  nothing.
- **One writer, by construction.** Only the launcher branch composes
  `HoldOrientation(Portrait)`. Leaving the launcher restores `FollowDevice`, which
  is the manifest's own unspecified orientation, so the device decides again.
- **The shape is still read from the window, never assumed.** A shell opened from a
  portrait launcher on a phone lying sideways turns as soon as it is shown.
- **It is a seam, `ScreenOrientation` (ADR-0013).** Applying an orientation policy
  is the OS's job: an Activity's requested orientation on Android, a scene's
  supported orientations on iOS. Which screen holds which policy is decided in
  `commonMain`.
