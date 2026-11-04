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

The second pass ran the same three commands and the Kotlin suite (595 tests as of this writing, all
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
