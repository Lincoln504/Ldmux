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

The Rust gate was run as well, because this pass touched only Kotlin and docs and the gate should
say so: `scripts/test-parallel.py` is green. It did fail **twice** while this work was being
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
