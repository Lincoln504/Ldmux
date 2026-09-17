# ADR-0011: One orientation per screen, decided in one place

**Status:** Accepted

## Context

The app has two shapes and they are not the same shape:

- the **menu** is a list, and a list is read the way you hold a phone — portrait;
- a **terminal** is a grid, and a grid wants width — landscape.

A phone cannot show both at once as two differently-shaped regions, and it is
worth writing down why, because "just slide between them with the other one
rotated" is the obvious idea and it will be proposed again.

## Decision

**The screen's orientation is decided in exactly one place**, from what is on
screen:

| On screen | Orientation |
|---|---|
| the menu | portrait, always |
| a terminal | landscape |
| a terminal, used as a *keyboard* | portrait, with the compact bar above the keyboard |

`lockOrientation` is called once, from `App`, keyed on that single derived value.
The previous arrangement had the orientation written per *terminal surface* — of
which several exist at once, because the pager keeps neighbours composed — so N
components were writing one activity's orientation. Harmless only while they all
happened to agree.

## Why not the other two

**Two orientations on screen at once is not possible.** Android gives an
*activity* one orientation; `screenOrientation` and `requestedOrientation` are
whole-window properties. Jetpack WindowManager's activity embedding does put two
activities side by side — but both are multi-window, and multi-window activities
have had their orientation requests **ignored since Android 12**, so it cannot
produce two orientations either. (The same rule is why this app should expect its
orientation lock to be ignored on large screens and foldables regardless of what
we ask for.)

**Rotating the content 90° inside a portrait activity is possible and is a trap.**
It is the only way to fake it, and it does make a true slide possible — but the
system bars and the soft keyboard both follow the *activity*, not the content. A
keyboard would come up as a strip across a sideways screen, and the status bar
would run down one edge. The terminal would have to be full-screen-immersive and
keyboard-less to hide both, which is a large amount of the app's behaviour given
up to make one transition prettier.

## What the rotation costs, and how it is paid

A configuration change recreates an activity by default, which would lose the open
pages on every flip. `MainActivity` therefore declares
`configChanges="orientation|screenSize|screenLayout|keyboardHidden|density|uiMode"`,
so a flip is a **relayout, not a relaunch**: no state is lost and there is no
black frame.

The transition is a slide of the content (`AnimatedContent`), on top of the
system's own rotation. It does not replace the rotation — nothing in the app can —
but it keeps the two screens moving through it, where a cut reads as a glitch.

## Consequences

- A second writer for the orientation is a bug, not a feature. If a future screen
  needs its own orientation, it adds a case to the one derivation in `App`.
- The manifest's `android:screenOrientation` is only the cold-start default; the
  app overrides it on the first composition. It must not contradict this table.
- Portrait-with-a-terminal *means* typing, so it is the only portrait terminal
  state and it always carries the keyboard. There is no portrait terminal without
  one, because a portrait grid that is not being typed into has no reason to be
  portrait.
- Orientation is the app's decision, not the sensor's: a phone set down at an
  angle must not reflow a terminal with a command half-typed into it.
