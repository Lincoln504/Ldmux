# The mobile UI: a critical audit of settings and the secondary screens

Written 2026-11-04. This is the record of a UX audit of the phone client's settings screen and the
secondary screens around it (the launcher, the mesh/radio screens and the modals), measured against
the rules the app already sets for itself. It names what was wrong, what was changed in the same
pass, and what is still a decision. Every claim carries a `file:line` so a later reader can check it
rather than trust it.

The scope word in the brief was "settings menu and RO pages"; the app has no read-only concept, so
"RO" is read as the **radio/route** pages — the mesh setup, the radio preset dialog and the launcher
— and the settings screen that contains them.

## 1. The standard this was measured against

The app states its own rules in comments, and most of them are good. The audit looks for places that
break a rule the app itself asserts:

- **one control height**, set by `LocalTouch` (`Theme.kt`, `LocalTouch`/`Touch`), reached through
  `MiniTextButton`/`IconBtn` so a density setting cannot reach some controls and not others
  (`Controls.kt`, `MiniTextButton`);
- **state is not colour alone** — the chosen-outline comment says a state "is a state, not an
  action" (`Theme.kt`, the `Chosen` comment);
- **a control that cannot act is absent or disabled, never present and inert** (`AppHeader.kt` "the button is then absent rather than present
  and inert"; `SettingsScreen.kt`, `TextSize`);
- **one dialog language**, hand-drawn, because Material's arrives in its own light scheme
  (`Modal.kt`, `ModalCard`);
- **the same word for the same act**, and the same act for the same word (the launcher's "close" vs
  the daemon's rule, `LauncherScreen.kt:157-163`);
- **no hidden actions on a row**: the launcher's own comment about "a choice made in one place and
  spent in another" (`LauncherScreen.kt`, the comment above `desktop?.detail`).

Where the app broke one of these, it is listed below as a defect; where it broke none and the choice
is merely arguable, it is in §5.

## 2. What was changed

Each item says what was wrong and why the change is the smallest one that fixes it.

### 2.1 Five pairs of options carried their state in colour only

`LinkChoice`, `PanelSide`, `ControlSize` (`SettingsScreen.kt`), the region row and the airtime row
(`mesh/MeshRadioSection.kt`) each drew two or four `MiniTextButton`s and painted the chosen label
`Accent` and the rest `Dim`. Three failures in one pattern: a screen reader has no state to read, a
colour-blind eye has no second signal, and a button whose text *is* the value cannot say whether a tap
sets it or reports it.

**Fixed** by one shared control, `OptionButton` (`Controls.kt`): the chosen option is a filled
`Line` fill with a `Chosen` outline (the theme's own name for "the outline of a control
that is chosen", `Theme.kt`) and
`Modifier.selectable(role = Role.RadioButton)` puts the state in the semantics tree. The label stays a
value; the shape says "you are here".

### 2.2 The screen's order contradicted the screen's logic

The sections read `host`, `link`, `display` (`SettingsScreen.kt:96-121` before the change). `host` was
the tailnet pairing — so the machine was read before the choice of how to reach it — and the mesh
pairing lived in a different section from the tailnet one. Choosing the route is the first question;
pairing is the second; and the two pairings are the same kind of thing.

**Fixed** by declaring the order once, as a value: `SettingsGroup` and `settingsGroups(hasMesh)`
(`SettingsScreen.kt`), tested in `SettingsLayoutTest`. Sections are now `link`,
`pairing`, `display`, with `link` absent where the platform has no second route (iOS), because a
choice of one is not a choice. Both pairings sit under `pairing`, each with a row label of the same
shape (`tailnet`, `mesh`).

### 2.3 Two headings on one screen at two sizes

`Section` drew an 11sp monospace `Dim` label; the mesh block drew its own `Text("mesh", 13sp, Fg)`
(`mesh/MeshRouteSection.kt`, the `Text("mesh")` heading), so the radio's half of the screen looked like a section and the
tailnet's looked like a row. **Fixed**: `mesh` is now the same 12sp `Dim` row label as `tailnet`, and
the gap *between* sections is wider than the gap within one, so grouping is carried by spacing as
well as by the label.

### 2.4 Two names for the same act, and three confirm strengths for destructive acts

- The tailnet row said `unpair`; the mesh row said `forget mesh pairing` (`SettingsScreen.kt`, `PairingRow`;
  `mesh/MeshRouteSection.kt`, the forget row). **Fixed**: both say `unpair`.
- Destructive actions had three strengths: `unpair` and `forget mesh pairing` asked twice, `unset`
  in the radio dialog fired on one tap (`mesh/MeshRadioSection.kt`, the preset dialog's dismiss row), and closing a shell used a
  dialog. **Fixed**: `unset` now asks once (`unset` → `unset?`), like its neighbours. The dialog for
  closing a shell stays, because ending a running program is a bigger act than forgetting a key.
- The armed state now uses `Danger`, not `Accent`: an armed destructive button was painted in the
  colour the app uses for "the primary thing to press" (`Theme.kt`, `Accent`).

### 2.5 Labels that named a state where a control belongs, and labels that hid what they did

| Before | After | Why |
|---|---|---|
| `lora` | `radio` | `lora` is a modulation the user never chose; every message the mesh route prints says "radio" |
| `tall` / `thin` | `comfortable` / `compact` | the setting changes control *height*; "tall" describes a shape rather than the trade |
| `airtime` `open`/`metered` | `budget` `metered`/`unlimited` | "airtime" is the quantity, "budget" is the choice; "open" reads as a verb |
| `new private` | `randomise` | what it does, in one word |
| `public…` / `hide public` | `public presets` / `hide presets` | no ellipsis: it expands the card, it does not open a surface |
| `tuple` | `preset` | the row writes a preset; "tuple" is the implementation's word |
| `1B` suffix | `path 1B` | a unit with no quantity |
| `forget mesh pairing` | `unpair` | same act, same word |
| `Settings` | `settings` | one capitalisation rule: lowercase except proper nouns |

### 2.6 A hidden action on the radio row

The radio's name was clickable to open the preset dialog while the button beside it opened the radio
picker (`mesh/MeshRouteSection.kt`, the radio row), so one row carried an invisible action and a visible one:
tapping the name to "select the radio" changed its *operating point* instead — which, on a radio that
then stops answering, is indistinguishable from having broken it. **Fixed**: the name is text, and
both actions are written down — `preset` and `change` (or `choose` when nothing is chosen).

### 2.7 A success drawn in the warning colour

`note` was one `String?` drawn in `Warn` (`mesh/MeshRadioSection.kt`, the preset dialog's `note`), so `applied` arrived in the
colour reserved for a fault (`Theme.kt`, `Warn`). **Fixed** with `Notice(text, ok)` and `NoticeLine`
(`Controls.kt`): good news in `Muted`, bad news in `Warn`.

### 2.8 A dialog whose primary button was a state, and whose confirm slot was "scan again"

`RadioPickerDialog` put `scan again` in `confirmButton` and `close` in `dismissButton`
(`mesh/MeshRadioSection.kt`, `RadioPickerDialog`'s buttons), and while a scan ran the confirm button read `scanning…` and still
restarted the scan. **Fixed**: one `ModalCard`, actions in the app's order (safe action first, the
acting one last), and `scan again` disabled while a scan runs.

### 2.9 Two dialog languages

`ModalCard` is hand-drawn *because* Material's dialog arrives in its own light scheme
(`Modal.kt`, `ModalCard`); yet four dialogs used `AlertDialog` with `containerColor` and `tonalElevation`
patched to hide that (`Dialogs.kt`, `mesh/MeshRadioSection.kt`). **Fixed**: all six modals are
`ModalCard`, so there is one dialog language, one padding, one scrim and one button row
(`ModalActions`).

### 2.10 The hand-drawn modal had no back gesture

`ModalCard` never registered a back handler, so with a modal open on the launcher the system back
reached the activity and left the app with the user's half-typed path in a field nobody would see
again. Material's dialog gave this for free; the hand-drawn one did not. **Fixed**:
`BackToLauncher(enabled = true) { onDismiss() }` in `ModalCard` (`Modal.kt`, `ModalCard`), so the topmost thing
on screen is what back closes.

### 2.11 A menu with one item

The launcher square in both bars opened `AppMenu`, a dropdown whose only item was `shells`
(`AppMenu.kt`, `KeyBars.kt`, `SidePanel.kt`) — two taps and a menu card to reach one screen,
and a "menu hiding a switch", which is the phrase the app uses against its own route dropdown
(`AppHeader.kt`, the note on the deleted route dropdown). **Fixed**: the square calls `onLauncher` directly and is labelled `shells`;
`AppMenu.kt` is deleted. The state that tracked the popup's focusability goes with it.

### 2.12 The unpaired screen told instead of offering

`AppContent.kt`, the launcher's `routeNotice` said "no host paired: scan a pairing code in Settings" and left the user to find a
cog in the corner — on the one screen where nothing else can be done. **Fixed**: the notice is a row
with a `scan a code` button that runs the same scan the settings row runs. The scanner's result *is*
the intent, so it does not ask twice (ADR-0017).

### 2.13 Contrast and type size

- `Dim.copy(alpha = 0.6f)` and `0.7f` were used for placeholders and explanatory lines
  (`Modal.kt` placeholder and help line; `SidePanel.kt` link diagnostics); over `Bg` those are about **2.8:1** and **3.6:1**, under
  the 4.5:1 that body text needs. **Fixed** with a named `Muted` (`#787878`, 4.5:1 on `#0A0A0A`), so
  the value is a colour someone can look up rather than an alpha someone computed.
- 9sp and 8sp text (`SidePanel.kt`, page count and link line; `Icons.kt`, `IconBtn` caption) moved to 10sp and 9sp respectively: nine
  point type on a phone is under the size at which letter shapes survive.

### 2.14 A label that hid the scope of its action

`unpair` on the `tailnet` row also forgets the mesh keys and the radio (`Link.kt`, `unpair()`, and
`unpairMesh` resets the route to the tailnet too). **Fixed in copy**: while armed, the row says "this
also forgets the mesh keys and the radio" when a mesh route exists, and the mesh row says "this
forgets the keys, the radio and the saved route".

## 2b. Second pass, same day: the scroll controls, the palette, and the system bars

The first pass was the settings screen and the chrome around it. This pass went through the terminal
surface, the launcher rows, the radio dialog and the Android shell. Each item is a defect with a
`file:line`, and each was changed in the same commit except where it says otherwise.

### 2b.1 The scroll buttons, and why the up arrow could do nothing

Reported from a field session: "the scroll up/down buttons were not working". Investigated in three
layers, because the answer differs per layer.

- **The engine is correct**, measured: a probe against `libghostty-vt` (`native/prebuilt/host`) fed
  30 lines into a 20×6 terminal and read the frame header back at each step.
  `bottom total=31 offset=25 len=6 active=1`, `up3 offset=22`, `top offset=0`, `down3 offset=3`,
  `alt offset=0 active=1`. So `scrollBy(-3)` really does move the viewport, the offset counts from
  the top of the scrollback, and the alternate screen really declines to scroll (which is why a
  swipe there goes to the program as wheel events).
- **The tailnet route works, and its first press is a page-in.** `TerminalController.scrollBy`
  reads the scrollbar before and after (`TerminalController.kt`), repaints, and then calls
  `maybeExtendHistory()`. At a fresh attach the daemon has sent the live screen only, so the first
  up-press has nothing above it to move — and the same press asks the keeper for a scrollback page.
  The reply is a whole-screen snapshot that rebuilds the engine and parks the view at the top of
  what arrived. So the control does something, but on a slow link the first press looks like
  nothing until the page arrives. That is a design consequence of "history arrives when you reach
  the top", not a dead control; it is written down here because it reads as a bug on a phone.
- **The mesh route had a real dead button, and it was two bugs.**
  - `MeshTerminalSource.scrollBy` returns 0 when the local history is empty, and *nothing said so*:
    the pad drew, the tap landed, and the screen did not change. Fixed by naming it —
    `scrollNotice` becomes `no history yet on this link`, or `start of history` when the view is
    already at the oldest row, shown for 2.5 s.
  - The history itself could stay empty for a whole session. `MeshHistory.noteShift` is the fallback
    for a host that cannot number its rows, and it refused any shift larger than **half a screen**.
    A build log, a `cat`, or anything that fills a screen between two commits therefore
    accumulated nothing at all. Fixed: a shift may now be as large as the overlap allows, provided
    every non-blank row in the overlap matches the new screen except at most one (the row being
    written now), and at least two real rows are in it. Ties go to the larger shift, because the
    smaller one only ties by excusing a row it cannot explain. Tests:
    `a screenful that scrolls at once still accumulates`, `one row being written does not stop the
    shift being read`, `a replacement with one row in common is not a scroll`.
  - Both routes' pads remain structurally reachable: the pad is a sibling drawn after the surface
    inside the page's `Box`, and the surface is a Compose `Canvas`, not a `SurfaceView`, so there is
    no separate window layer for it to hide behind. **No device was involved in this pass**; the
    engine probe and the unit tests are the evidence, and the on-device tap is still unverified.

### 2b.2 Two palette values that did not do what their comments said

Measured with the WCAG relative-luminance formula against the real tokens, not estimated.

| Token | Was | Ratio then | Now | Ratio now |
|---|---|---|---|---|
| `Muted` | #787878 | 4.48:1 on `Bg` (**under** the 4.5:1 floor, while the comment claimed "4.5:1, which is the floor"), **3.94:1 on `Surface`** — where its explanatory text actually lives | #888888 | 5.58:1 on `Bg`, 4.91:1 on `Surface` |
| `Dim` | #8C8C8C | 5.18:1 on `Surface`, 4.04:1 on `Raised` | #A0A0A0 | 6.66:1 / 5.19:1 |
| `Line` used as a control outline | #2A2A2A | **1.21:1 on `Surface`, 1.38:1 on `Bg`** — an "outlined" button outlined in something you cannot see | `Line` stays for dividers; new `Edge` #686868 | 3.55:1 / 3.12:1 |

`Muted` is never drawn on `Raised`; `Dim` is, as icon glyphs on the filled icon button, which is why
it moved too. `Chosen` (#9A9A9A) is unchanged: it is an outline, and it stays brighter than `Dim`'s
new value only in role, not in luminance — a relationship worth revisiting if the two ever appear
side by side.

Also fixed in the same class: **a disabled `AccentBtn` was a `Surface`-coloured box on a `Surface`
modal**, i.e. no shape at all, with only a dim label floating. It fills with `Line` now, the fix
`OptionButton` already documented for itself.

### 2b.3 The status bar was invisible on a phone set to light mode

`MainActivity` called `enableEdgeToEdge()` with no arguments. The androidx helper's default is
`SystemBarStyle.auto`, which chooses **light or dark bar icons from the system's own light/dark
setting** — and this app is dark-only. On a light-mode phone the clock, the battery and the gesture
pill were drawn dark on a #0A0A0A app. Fixed with `SystemBarStyle.dark` for both bars, and
`windowBackground` set to the app's own #0A0A0A (it was pure black, a visible 10/255 step at launch
and in the recents thumbnail). The `statusBarColor`/`navigationBarColor` items in `themes.xml` were
dead — edge to edge overrides them — and are gone with a note saying why.

### 2b.4 The row that is the app's primary selection control announced nothing about being chosen

`ChoiceRow` (the launcher's shell rows) was a bare `clickable`. A screen reader got "clickable" and
no state, while being chosen lived only in a drawn tick and a numeral. It is
`toggleable(role = Role.Checkbox)` now, so the role, the state and the action are in the semantics
tree, and the tick-box numeral keeps the visual.

### 2b.5 Long names stopped mid-character with no sign

`TextOverflow.Ellipsis` was used at one site in the whole app. The launcher's title and subtitle, the
paired machine's name in Settings, the radio row's name and address, and the option labels now
ellipsize. The radio row matters most: a truncated MAC address makes two radios indistinguishable,
which is the one thing that row exists to prevent. **Not done:** the other twelve `maxLines = 1`
sites (the key bars and the radio dialog's short fixed labels), where truncation cannot lose
meaning.

### 2b.6 `folderName("")` said the shell was at the filesystem root

`Icons.kt` returned `/` for an empty cwd, so the shell header's title asserted a location nobody had
reported. It answers `shell` now — the one thing that is true when the folder is unknown.

### 2b.7 The radio dialog could lose "restore" permanently

`MeshRadioSection` used one flag (`seeded`) for two facts: "the radio's tuple has arrived" and "the
user has touched this dialog". `choose()` set it, and the seeding effect returned early when it was
set — so tapping a region or `randomise` before the radio answered meant `original` stayed null for
the life of the dialog: `restore` disabled forever, the "current …" comparison never drawn, and
`apply` enabled with no idea what it was replacing. Split into `seeded` (the radio answered) and
`userTouched` (the user chose first, and their choice wins).

### 2b.8 Faint text was carried by the frame and never drawn

`ESC[2m` arrives as frame flag bit 3 with the foreground colour untouched, so the client owns half
of the attribute. `Cell.drawnFg` ignored it, which meant every program that de-emphasises text
(`ls` extracts, `fzf` rows, a diff's context) was painted at full strength. It blends halfway to the
cell's own background now, which is also correct on a reverse-video cell because the drawn channels
are already swapped. This surfaced while making the public-channel echo faint (ADR-0026); it was a
rendering defect on its own.

### 2b.9 Three more, found while perfecting

- **The modal's card did not stop taps.** `ModalCard` shielded the card with
  `clickable(enabled = false)`, and a disabled `clickable` does not consume in Compose foundation —
  so a tap on the card's own title or padding fell through to the scrim and dismissed the dialog,
  taking a half-typed path with it. The card now consumes with the `pointerInput` loop
  `HeldElsewhere` already uses, and the scrim is a `detectTapGestures` target rather than a
  `clickable`, which also removes an unlabelled full-screen button from the semantics tree (it used
  to merge the dialog's contents into itself). Verified by reading the foundation contract, not on a
  device; back and the card's buttons remain the reader-reachable ways out.
- **A daemon error could fill the screen.** The toast drew `err.message` with no line limit in a Row
  with no width limit, so a rejected request that quoted a path grew to cover the terminal it was
  about. Four lines and an ellipsis.
- **Two rows of the radio preset table were wrong against the feed they claim to mirror.** `Australia
  narrow` was CR8 where the feed says CR7 (the coding rate does not change who hears whom, but the
  airtime meter is computed from it and that number gates a send), and `New Zealand narrow` asked
  for 1-byte path stamps where the region uses 2 (the width is what a node stamps on what it sends,
  so a 1-byte stamp is a shorter trace than neighbours read and a bigger payload budget than the
  region allows). `USA`'s path width is now documented as *the firmware default* rather than a value
  the feed states, because the feed carries `null` there. **Still open:** nothing tests the table
  against the feed, and the feed fixture lives in the other project (`meshmore-sns`), so the check
  would have to copy it.

### 2b.10 The sync mode's own state machine, audited

Six defects in ADR-0026's implementation, four of them user-visible on the link the mode exists for:

1. **A sync sent only its first record's worth.** `PublicChannelHold.painted()` cleared `flushing`
   as well as `awaiting`, contradicting its own neighbour's doc ("clears when the buffer is empty,
   not when the answer arrives"). A screen arriving mid-drain — the normal case, since an `Input`
   costs a record and the ARQ queue holds two — closed the drain and left the rest of the text held
   with nothing in flight. A two-line command sent its first line. A paint now ends only the wait;
   the drain ends when the buffer empties, at the deadline, or on leaving the mode.
2. **The "no answer yet" notice cleared itself.** The deadline effect was keyed on `awaitingSync`,
   which the deadline's own action sets false — so the write re-keyed the effect and the relaunched
   body nulled the notice. The ADR's promised line rendered for one frame. Keyed on a press count
   now, which also fixes a second press inheriting the first press's remaining clock.
3. **A shell switch sent shell A's held text to shell B.** `clear()` dropped the held screen but not
   the buffer, which lives in the handle. Discarded on a change of shell id — not on `clear()`,
   which also runs on a rotation's `Resize`.
4. **The echo did not survive a paint**, so a sync answer erased text the bar still counted. The
   source remembers it and every paint re-applies it (never on the alternate screen or in history).
5. **Held text was written into the engine unsanitised.** Host rows go through `cleanRow`; the echo
   path had no such filter, so a pasted `ESC[2J` executed against the phone's copy of the host's
   screen. C0/DEL/C1 are dropped now except the four the echo itself builds.
6. **Rows that scrolled while frozen were dropped from scrollback** — `show()` kept the newest screen
   without noting the pair to `MeshHistory`, so a build log watched through a freeze could not be
   scrolled back past it. Noted once per pair now.

Plus four smaller ones: the echo and the count lagged up to 120 ms behind typing (`settle` refreshed
only the literal line); "view N behind" was shadowed by "nothing held"; "too much held: sync to send"
named a button that does nothing when the refusal left the buffer empty; `discard` was disabled for
the whole 20 s wait — exactly when a dead link makes it the only control worth pressing; and the
record count divided by 96 where the sender backs off to a character boundary, so "about 2 records"
was occasionally 3. The ADR's "no fake can outlive the next truth" was an overclaim — while frozen,
nothing paints — and is now "the next paint".

### 2b.11 Two more on the sync bar

- **The radio's channel is now read from the radio.** The classification was written only by the
  settings radio dialog, so an install that never opened it ran on the cautious default — public —
  and was *forced* into held typing while the bar asserted "the radio is on a public channel", which
  on a private tuple is false. Forcing a mode on a guess is the exact symptom ADR-0026 warned about.
  The link reads `SELF_INFO` as part of its own handshake, so the terminal screen hears the answer
  and saves it; `connectionKey` excludes the field, so saving does not rebuild the connection. A
  radio that turns out private does not turn the mode off by itself — leaving stays the user's call.
- **The sync bar's status is a polite live region.** "sending…" → "waiting for the answer" → "2
  characters" is the only place the mode says what it is doing, and it changes without the user
  touching the control that caused it, so a reader would otherwise hear it once and never learn the
  send finished. A polite region waits for the current utterance rather than interrupting.

### 2b.12 The radio dialog showed none of what it read

The dialog opened straight onto "region". Every fact it needs was already read — `SELF_INFO` on the
handshake, `DEVICE_INFO` on request, `MeshStatus` for signal and battery — and the one screen about
the radio answered "which channel" without saying which radio, what firmware, or whether it was in
range. It now shows up to three dim lines: name and firmware, signal and battery (with the radio's
`-128`/`255` sentinels rendered as words, as `ldmux mesh status` does), and power against the radio's
own maximum plus path width and contact capacity. Each line appears only when its source answered,
so an unbound radio shows nothing rather than zeros.

**What the screen deliberately does not offer**, and why: the *name*, *channel slots* and *advert
position* are MeshMore's on these boards, and two apps writing the same field is how a radio ends up
named something neither user chose. The operating point — the tuple — is what LDMUX owns, and it is
the only control offered. Also corrected: the vendored binding's doc comment called
`CMD_SET_ADVERT_LATLON` `0x0C`; the opcode table and the codec both say `0x0E` (0x0C is TX power).
A comment-only defect, but it is the kind that sends the next reader to the wrong byte.

### 2b.13 The app can be told the host's channel

The chicken-and-egg of a two-radio mesh: the phone cannot learn the host's tuple over the link,
because it needs to be on that tuple to reach the link. The dialog offered a region preset or a
random private one — enough to *pick* a channel, and no way to *join* the one the host is already
on. A host radio is configured once and stays put; the phone is the thing that must land on it.
So the dialog now has a `custom` field: one line, four numbers, in the order the firmware prints them
(`910.525 500 7 5`, units and commas accepted), with power and path width kept from the current
selection because they are not part of "which channel". A text that does not parse disables `apply`
rather than committing the previous selection — a control that changes a radio's frequency must not
act on something other than what its field says. `parseRadioTuple` is a pure function with its own
tests, including the round trip through `radioTupleText`.

**The better fix needs three repos**: carrying the host's preset in the pairing code would remove
the guess entirely. The gateway knows the preset and prints it, but its protocol does not expose it;
so the code cannot carry it without a gateway change (in `meshcore_host`, a different repository).
Until then, the operator reads the preset from the gateway's start-up line — or from this app, which
now reads its own radio's tuple and can be typed into.

## 2c. Third pass: rotation, the landscape panel, and the controls that vanished

Reported: "the row/layout placement looks great, but the system does not seem to respond correctly
to a rotation switch. Rotation should request a new view the same way the view/prev-next buttons
do." And after that: "buttons should always stay, greyed out when they are not touchable right now,
not fully disappear", and "in horizontal mode put these in the scroll area, at the top rather than
the bottom".

Everything below was measured on the device against the mesh route, with the app's own log lines
(`appLog`, tag `ldmux`) as the evidence. Before this pass the mesh route logged **nothing** at all —
no grid, no profile, no commit — which is also why the first finding took an instrumented build to
see. The tailnet route has logged its grid since it had one; the mesh route now logs its grid, its
profile, its commits and the surface's viewport.

### 2c.1 The landscape panel was as wide as the screen, so the terminal had no width

The mesh route wraps `SidePanel` in a `Column` so the mode's row can sit under the panel's keys.
That wrapper had no width, and its children did (`Box(Modifier.fillMaxWidth())`,
`barRow(Modifier.fillMaxWidth())`), so the wrapper measured at the Row's full width and the
terminal's `Column` — the Row's weighted child — was measured at **zero**.

Measured, portrait to landscape on the device, before the fix:

```
surface 910cebb8aa viewport 0x131
surface 910cebb8aa viewport 0x950
```

and nothing after it: no `mesh grid` line, because `setGrid` returns early on a zero-width pane, and
no repaint, because the surface had nothing to draw into.

The visible result was a black terminal with the panel's `Surface` colour across the whole screen
(sampled: `#1A1A1A`, the panel's background, at x = 2000 of 2400), and no line explaining it: the
"waiting for the shell's screen" line only draws when nothing has been drawn at all, and an old
screen *had* been drawn into a zero-width surface. The tailnet route has no wrapper — its panel is
`SidePanel` itself, which carries `PANEL_WIDTH` — which is why only this route went blank.

**Fixed by deleting the wrapper**, not by giving it a width: the mode's row is now a slot *inside*
the panel's scrolling area (`SidePanel(modeRow = …)`), which is also what the second request asked
for. The row is at the top of that scroll, above `paste`, where a control the user presses while
reading belongs.

### 2c.2 Rotation asked the host for a new grid, but the bookkeeping could skip it

Rotation's request and the header's ◀ ▶ buttons are two different requests and both are right:
the arrows send an `Attach` for another shell (a new view of a different pane), while a rotation
sends a `Resize` and a `Resync` for the shell already attached — a rotation must not tear down the
pty or lose what the pane had. That request existed. What could lose it was the guard that decided
whether to make it:

```kotlin
if (profile.cols == cols && profile.rows == rows) return   // the host's *last answer*
```

That compares the phone's measurement against the host's **last reported** size, and on this link
the report is an answer to a request that is still in flight. Rotate portrait → landscape →
portrait inside one round trip and the second `setGrid` finds the *old* profile (portrait) equal to
the new grid (portrait), sends nothing, and leaves the pty at the landscape width while the phone
paints the portrait one. The tailnet's `setGrid` never had this fault because it compares against
what it **asked for** (`wantCols`/`wantRows`), and the mesh route now does the same: the same grid
for the same shell is asked for once (so a host that clamps cannot start a loop), and the host's
"already right" is only trusted before anything has been asked on that connection.

The request can also be *dropped*: `queue()` returned nothing and the outcome was discarded, so a
`Resize` that hit a full session queue vanished and the pane kept the old grid for the life of the
page. The queue now reports its outcome, the fit is retried from `tick()` when the request did not
go, and `MeshTermDriverTest` pins all three rules (a rotation back to an unanswered grid asks
again; the same grid is asked once; a refused resize is asked again).

### 2c.3 A rotation that took longer than eight seconds threw the user out of the shell

The mesh screen leaves the held screen for the shell list when the link has been down for
`LINK_FAILURE_GRACE_MS` (8 s, the tailnet's value). This link's own verdicts are slower than that:
the driver calls a session dead after **20 s** of silence and retries its first handshake at **8 s**,
so the grace fired *inside* a retry that was still working. Measured on the device: a rotation whose
resize answer was still in flight was declared a dead link, the screen went to the list, and — the
list being portrait-only per ADR-0011 — the phone **rotated itself back to portrait** under the
user. The report's "does not respond correctly to a rotation" is exactly this: the rotation worked,
and then the app undid it.

Fixed in two places: the mesh route's own grace is `MESH_FAILURE_GRACE_MS = 30_000` (longer than the
driver's 20 s silence rule and its 8 s retry, so only a real verdict leaves the shell), and the
system back gesture is enabled while the screen is holding a dead link — it used to require
`terminalLive`, so the one control that could leave a frozen screen with no header, no keys and no
bar was switched off exactly when it was the only way out.

### 2c.4 The mesh route's shell list did not hold portrait

ADR-0011 makes the launcher portrait-only on both routes. The tailnet route composes
`HoldOrientation(Portrait)` for its list; the mesh route never did, so the same screen turned over
on one route and not the other. `if (showList) HoldOrientation(OrientationPolicy.Portrait)` — the
terminal itself still follows the device, and leaving the list restores `FollowDevice` by
construction.

### 2c.5 Controls that vanished instead of dimming

The mode's row collapsed to a lone `hold input` button whenever the mode was off, so the status
line and the **sync button were absent in exactly the state a user looking for sync is most likely
to be in** (the mode was off and they had not noticed). It also moved its one remaining control from
the left edge to the right when the mode came on. The row now draws all three things in both
states: the status, the sync button, and the mode's own switch. What cannot act is drawn in `Dim`
and is not clickable — never removed.

The same rule applied where else it bit:

- `SidePanel`'s `voice` key was absent where the link carries no speech channel (the mesh route);
  it is now drawn dim and inert, so the key row keeps its shape between routes.
- `KeyBtn` gained `enabled`, which dims the label and drops the click. No key row reflows when a key
  becomes unavailable.

### 2c.6 The row has two shapes, because it lives in two widths

Portrait gives it the whole screen under the terminal; the landscape panel gives it 166 dp of
content width. Three things do not fit in 166 dp side by side: measured, an even split drew
`hold ir`, a 1.3 share drew `hold in…`, and `TextOverflow.Ellipsis` then made the truncation honest
but still useless. Narrow stacks: the status (wrapping to two lines when it must), then `show`, then
`hold input`, each full width. That also gives each control a thumb-sized target rather than two
halves of one, and it is what the panel shows on the device now.

### 2c.7 What the radio link was doing meanwhile, and one cause removed

The device pass could not have found any of the above without the link staying up, and it did not
stay up. The host gateway was restarting **every two minutes** with `nothing heard off the air for
two minutes; rebooting the radio and exiting` (exit 75), each restart dropping the phone's link. Its
deafness rule judged a port by "opened N minutes ago and heard nothing off the air", which is also
the description of a healthy idle link: the daemon sends nothing while the phone sits at a prompt.
The rule now requires a **send that went unanswered** — silence with nothing asked is not evidence —
and with it the gateway stayed up for the rest of the session (no restart between 21:28 and the end
of the pass, against one every ~2 minutes before). That fix is in `meshmore-sns`
(`packages/meshcore_host/lib/src/serial_mesh_sender.dart`), with its own tests.

What remains on that link is a genuine asymmetry under load: the daemon reported `53 sent` against
`5 bytes received` over one session, so phone → host records were not arriving while host → phone
ones were. That is radio, not layout, and it is recorded rather than diagnosed here.

## 2d. Fourth pass: the settings page, the two input modes, and a reconnect that lands home

Reported: "fields are being enumerated and it is super unclear — it is like saying Debian or
Meshcore right next to the name of the setting, and things are just thrown on the page." And
separately: two modes, **live** and **manual sync**, with the mode chosen in Settings beside the
channel, no leave button in the terminal, and the sync controls gone in live mode. And: a long-idle
shell that announces "the host did not answer" and stays there until refresh.

### 2d.1 Every row had its label and its value run together

`tailnet  debian  unpair`, `radio  WIOld  AA:BB:CC  preset  change`, `code  scan  unpair`. There was
no column and no divider, so the eye had no place to stop between the name of a setting and its
value, and two adjacent facts read as one phrase. The screen now has one shape: a section heading
with a hairline under it, then rows whose labels sit in a fixed 92 dp column and whose values line
up on one vertical line down the page. Consequences move to a muted line under the row they explain,
indented to the value column (`SettingsRows.kt`, used by `SettingsScreen` and `MeshRouteSection`).

Two rows needed more than that:

- **The value can be two lines.** `914.25 MHz  500.0 kHz  SF8 · CR5` and `0.0 s of 36 s this hour`
  were both cut with an ellipsis, and the part cut was the part the user opened the screen to read.
  A measurement may wrap to two lines; a name stays one, because a wrapped name makes one row into
  two.
- **The radio's facts are read, not remembered.** `preset` and `airtime` come from the radio itself
  (`MeshRadioFacts`), so a value in the config cannot drift from the hardware. Airtime is the
  duty-cycle figure the budget is spent against, and the records line is what is left of the hour.

### 2d.2 Two modes, and the terminal's own switch removed

`meshInputMode` is `live` or `manual sync`, persisted by name (not by ordinal), and chosen in
Settings next to the channel. `meshHoldsInput` is the one derived value the terminal reads: a public
channel forces manual sync (ADR-0026), a private one takes the user's choice. The label was changed
from "typing is your choice" to "mode is your choice" at the user's request, because the choice is
about the mode, not about typing.

In **live** mode the terminal now draws no mode row at all: there is nothing to hold and nothing to
release, and a sync button would be a control for a mode that is not running. In **manual sync** the
row is drawn in every state, dimmed where it cannot act. The `hold input` / `leave` pair is gone from
the terminal: leaving a mode is changing a setting, and a connection-level decision does not belong
on a per-screen row next to the keys.

### 2d.3 A link that dropped needed a manual refresh

Three separate faults, all on the failure path, and each one alone was enough to strand the screen:

1. **A failure set the view and kept looping.** `transmit` (a radio refusal), `pump` (a session
   exception) and `onRecord` (a handshake record that could not be finished) all did
   `terminalView = LinkDown(...)` and returned. They never set `transportDead`, which is the only
   thing that ends `run`'s loop, so the caller's rebuild — the retry — was never reached. The screen
   said "the host did not answer" and stayed there until the refresh button built the connection it
   had stopped building. All three now go through `failed`, the one place that ends a connection.
2. **`NoPath` and `Refused` stopped the retry for good.** The rebuild effect treated them as states a
   retry cannot cure. Both are the radio's word about *one record* — a busy transmit table, a path
   that has not resolved yet — and they are the two most likely transient failures on this link. The
   set is now `NeedsRadio` and `NeedsPermission`, which are genuinely the user's to fix.
3. **The ladder stopped climbing at thirty seconds and the screen could give up.** It now climbs
   2, 4, 8, 16, 32, 64, 128, 256 seconds and stays there (`rebuildCapMs`, tested). A handshake costs
   several records against an hourly budget of dozens, so the cap is deliberate: five minutes at
   worst to pick a returning link, one handshake every four minutes while it is gone.

### 2d.4 A handshake lands on the shell list, and the words stopped accusing the host

A rebuilt connection is a new session, so the screen now lands on the **list**, not the shell it was
in: the host may have restarted and the shell may be gone, and the list is the truth about what it
has now. Measured on the device after this change: a link dropped while a shell was open, no refresh
was pressed, and about ninety seconds later the list had refilled by itself with every shell.

The message was the other half of the problem. "the host did not answer: it is off or out of range,
both radios are on different presets, or it does not know this phone" asserted a diagnosis on the
first failed record, before the app had retried once. `MeshBody` now carries a short line and a muted
detail: **"the link dropped"**, then "this screen keeps retrying by itself; refresh to try now. If it
never connects: ...". The checklist is still there for a link that never comes back, but it is no
longer the first thing a user reads while the app is already recovering.

### 2d.5 What the device showed

- **Settings**, read back through `uiautomator`: every label at x=45 and every value at x=275, the
  preset and the airtime on two lines, `channel  private · mode is your choice`,
  `input  live  manual sync`.
- **The mode persists by name**: tapping `manual sync` wrote
  `<string name="mesh_input_mode">ManualSync</string>`. In live mode the terminal drew **no mode
  row** at all; in manual sync it drew `view 6 behind` and a filled `show`, which became
  `1 key, 1 record · view 42 behind` with `sync` once input was held. No `hold input` and no
  `leave` anywhere.
- **The reconnect landed home twice with no refresh pressed.** Once from the terminal's own list
  button (the link dropped, "handshake · 10 s" for a few seconds, then the full list) and once while
  the screen sat untouched after a settings visit: twenty seconds after a dump that read only
  `handshaking · 10 s`, the same list held every shell. That is the reported complaint cured: the
  app rebuilt the connection itself and came back to the home screen.
- **Not verified this pass**: switching between two ticked shells (the link dropped while the second
  shell was opening, and a later attempt reopened the list), and the landscape stacking of the
  sync row (it is the same `BoxWithConstraints` branch measured in §2c.6, and the code did not
  change). Both are open items below rather than claims.

### 2d.6 One defect found by looking at the screen

`preset` and `airtime` said "not read" and never filled in, and the Bluetooth log showed the GATT
link opening and being torn down again within eight seconds. The cause was in the composable, not the
radio: `DisposableEffect(link) { onDispose { link?.release() } }` captures a **delegated state
property** and reads it at dispose time, when it already holds the *new* value. The effect created
for `null` disposed after the acquire and released the link that had just been acquired. The value
is captured in a local now, in both places that had the pattern.

## 3. How this was verified

The Kotlin toolchain in this environment needs an explicit JDK: the default
`/usr/lib/jvm/java-21-openjdk-amd64` is a JRE and Gradle reports "does not provide the required
capabilities: [JAVA_COMPILER]". With a JDK 21 whose `bin/javac` exists:

```
cd packages/mobile
JAVA_HOME=<jdk-21> ./gradlew --offline :composeApp:compileDebugKotlin   # ~9 s incremental
JAVA_HOME=<jdk-21> ./gradlew --offline :composeApp:testDebugUnitTest    # the Kotlin unit tests
JAVA_HOME=<jdk-21> ./gradlew --offline :composeApp:assembleDebug        # the APK (resources, manifest)
```

`scripts/android.sh test` is the documented entry point and does the same thing inside
`nix develop .#mobile`. Everything above was run; the unit suite passes and the APK assembles. **No
device was involved**: nothing here is a claim about how the screens look or feel on a phone.

The second pass ran the same three commands and the Kotlin suite (605 tests as of this writing, all
passing), plus the native engine's own suite in `packages/mobile/scripts/test-native.sh` — 290 checks
and 16 JNI checks — because the scroll investigation needed the engine's real behaviour rather than a
reading of it (`LDMUX_VT_LIB_DIR` and `LDMUX_VT_INCLUDE_DIR` pointed at `native/prebuilt/host`, which
is what makes the script runnable outside a nix shell).

The Rust gate was run as well, because the first pass touched only Kotlin and docs and the gate should
say so: `scripts/test-parallel.py` is green. The second pass touched no Rust, so it was not re-run. It did fail **twice** while this work was being
verified, in two different binaries, both on a time-bounded wait under pool load —
`handover_size_and_close_does_not_detach` (the keeper's socket wait) and
`desktop_e2e::a_window_is_opened_found_placed_and_closed_under_i3` (`i3 never answered`). Each
passed alone immediately afterwards and the mechanism was not proven, so both waits were widened
(10 s to 40 s, 15 s to 45 s) and the observation is recorded beside them rather than claimed as a
fix. A flake in the gate is a gate that cannot be trusted, so a third occurrence is worth chasing
rather than widening again.

The new `SettingsLayoutTest` pins the two decisions a later edit can silently invert: the section
order, and that the link's consequence line does not reintroduce the word `lora`.

**The third pass is the first one with a device in it.** Every measured claim in §2c comes from the
phone (`adb` + the app's `ldmux` log tag) against the mesh route, with the host-side truth read from
`ldmux mesh status`. What it establishes is *behaviour*: which records the app asked for, at which
grid, and what it drew afterwards. It still says nothing about whether a screen looks right; that
needs eyes, and this pass had them only as screenshots.

The same three Gradle commands ran, plus `:composeApp:testDebugUnitTest --rerun-tasks` (**619 tests,
0 failures, 0 errors**) and the native suite (`16 checks, 0 failures` in the non-sanitized run).
Three tests in `MeshTermDriverTest` pin the grid rules of §2c.2, four more pin the failure paths of
§2d.3 (a radio refusal, a polling exception, a handshake that cannot finish, and a list asked for
once while in flight), and `MeshReconnectTest` pins the ladder and the mode's truth table.

The radio work in §2c.7 is in the other repository and was verified there: `dart test` in
`packages/meshcore_host` is 39 tests green, including two rewritten deafness cases and one new one
("an idle link is not deaf").

## 4. What is still open, and why it is not fixed here

- **Opening settings ends the session you were in.** `App.kt`, the `showSettings` branch, draws settings *instead of* the
  route, so `MeshTerminalScreen` leaves the composition and its `onDispose` calls `conn.close()`
  (`mesh/MeshTerminalScreen.kt`, the `DisposableEffect(conn)` that calls `conn.close()`), and on the tailnet the terminal pages detach. Coming back
  rebuilds the connection: on the radio that is a fresh handshake, which is minutes. **Both sides are
  defensible**, which is why it is not changed: the radio preset dialog *needs* the radio, and a live
  session holds the BLE link, so closing the session while settings is open is what makes "change the
  preset" possible at all. The fix is architectural — hoist the connection above the settings branch
  and suspend it rather than close it — and it is a decision, not a tweak.
- **`RECONNECT_GRACE_MS` and the settings teardown interact.** See `HIGH-LATENCY.md`, F2: the grace is
  shorter than the client's own reconnect above about 1.3 s RTT. A settings visit makes a reconnect
  more likely, not less.
- **The settings screen has no scroll affordance.** It is a `verticalScroll` with no indicator; on a
  short screen the third section is off the bottom with nothing saying so. A scrollbar or a fade is a
  design choice.
- **The text-size control is 21 taps end to end** (7sp to 28sp in 1sp steps, `Config.kt:143-145`). A
  slider or press-and-hold repeat would be better; both are larger changes than the audit's remit.
- **`AppHeader`'s title is the app name on both list screens** (`ldmux`). On a landing screen that is
  a convention; if the list ever gains a second purpose it will need to say which one.

## 5. Swept and deliberately left alone

These read as oddities and are not defects; recording them keeps the next audit from re-opening them:

- **`+ shell` is a row in the launcher's list** rather than a button beside `open`
  (`LauncherScreen.kt`, the `+ shell` item). It is one more thing you can open, and it sits where the list ends.
- **`open` is inert and dimmed with nothing chosen** (`LauncherScreen.kt`, `OpenBar` and its call site). A live button that
  does nothing is the worse failure.
- **The tick box becomes the page number** (`LauncherScreen.kt`, `ChoiceRow`'s tick box). Order is the page order and
  the number is the same number the pager's header shows.
- **`+ shell` opens the login shell only, with no command field** (`Modal.kt`, `NewShellModal`). The daemon has
  no "choose the program" affordance, so a field would be a second way to do a thing that does not
  exist.
- **The mesh screen's list header carries settings, the terminal view does not** (`mesh/MeshTerminalScreen.kt`, the list screen's `AppHeader`). The header belongs to the list, as on the tailnet.
- **`ChoiceRow`'s in-use shade is `Accent.copy(alpha = 0.09f)`** (`LauncherScreen.kt`, `ChoiceRow`'s background). A tint on a
  row background is decoration rather than text, so the 4.5:1 rule does not apply to it.
