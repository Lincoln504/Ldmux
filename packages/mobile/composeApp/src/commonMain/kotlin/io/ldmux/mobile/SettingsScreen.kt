package io.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.safeDrawingPadding
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.ldmux.mobile.mesh.MeshRoute
import io.ldmux.mobile.mesh.MeshRouteActions
import io.ldmux.mobile.mesh.MeshRouteSection
import io.ldmux.mobile.mesh.PairingNote
import io.ldmux.mobile.mesh.canUnpair
import io.ldmux.mobile.mesh.pairingNote

/**
 * Settings, as a screen.
 *
 * It was an `AlertDialog`, and a dialog is the wrong shape for this: the mesh route's setup is a
 * radio picker, a preset and a set of credentials, and a modal card over a terminal is a place to
 * read one row, not to do that. It is also the only way to switch which link the app is on, and a
 * mode is a screen-sized decision.
 *
 * **Edits apply as they are made.** The dialog had a `save` button because a dialog has to be
 * able to be dismissed; a screen that returns to the launcher has no such need, and a pairing, a
 * radio pick and a font size all want to be visible immediately. Every control therefore calls
 * [onSave] with the whole config, and the caller persists it.
 *
 * There is no link switch on the launcher: this is where the link is chosen, and the launcher
 * then shows the app on that link only. The mesh section is absent where the platform has no
 * mesh route (Apple: tailnet only), which is decided by the caller and by
 * [MeshRouteSection]'s own `meshExists` check rather than guessed here.
 */
@Composable
internal fun SettingsScreen(
    current: LdmuxConfig,
    onBack: () -> Unit,
    /** Persist a change now. The screen never keeps a draft. */
    onSave: (LdmuxConfig) -> Unit,
    /** A good scan: pair and stay where the user is. Pressing `pair` was the question. */
    onPaired: (Pairing) -> Unit,
    /** Forget the pairing now (ADR-0017). */
    onUnpair: () -> Unit,
    /** The mesh route's setup, or null where this build has no mesh route. */
    mesh: MeshRouteActions? = null,
) {
    // **One pairing can change another block's facts.** A scan in the `pairing` row can store the
    // radio's keys, and the radio block below reads secure storage rather than the config: it used
    // to capture that reading once, so right after a successful scan the row said "· radio" while
    // the block underneath still said "scan a pairing code made with `ldmux code --mesh`". The
    // counter is bumped by every pairing and is the block's remember key. Not `connectionKey`: a
    // mesh-only code does not change it, and that key carries the token.
    var credentialsVersion by remember { mutableStateOf(0) }
    val meshPresent = mesh != null
    // What secure storage holds, re-read whenever a pairing (or an unpair) happened. The config
    // cannot answer this: the keys are in the Keystore, not in [LdmuxConfig].
    val radioPaired = remember(mesh, current.connectionKey, credentialsVersion) {
        mesh?.credentials()?.complete == true
    }
    var pairProblem by remember { mutableStateOf<String?>(null) }
    var pairNote by remember { mutableStateOf<PairingNote?>(null) }
    var pasteOpen by remember { mutableStateOf(false) }
    // **One place where a pairing lands**, whether it came from the scanner or the field. Both
    // return a [Pairing], and both have the same three endings: the tailnet changed (the config
    // says so), the radio was enrolled but the route is still the tailnet (nothing on screen
    // changes), or the keys could not be stored (the row would say "paired" and the radio half
    // would be empty).
    val acceptPairing: (Pairing) -> Unit = { p ->
        pairProblem = null
        onPaired(p)
        // Bumped *after* the store call: this is what makes the block below re-read storage.
        credentialsVersion++
        pairNote = pairingNote(
            p,
            radioKeysStored = mesh?.credentials()?.complete,
            route = current.route,
            hasMeshRoute = meshPresent,
        )
    }
    // The system back gesture means what the `back` button means, and it must not be allowed to
    // fall through to the activity: leaving the app from a settings screen that is one tap from
    // the launcher reads as a crash.
    BackToLauncher(enabled = true) { onBack() }
    Column(
        Modifier
            .fillMaxSize()
            .background(Bg)
            .safeDrawingPadding()
            .imePadding(),
    ) {
        Row(
            Modifier.fillMaxWidth().padding(horizontal = LocalTouch.current.rowPad * 3, vertical = LocalTouch.current.rowPad * 2),
            verticalAlignment = Alignment.CenterVertically,
        ) {
            MiniTextButton(onClick = onBack) { Text("\u2039 back", color = Accent, fontSize = 14.sp) }
            Spacer(Modifier.width(12.dp))
            // Lowercase, like every other label in the app and unlike a document title. The
            // screen said `Settings` while its own three headings said `link`, `pairing` and
            // `display`, so one screen carried two capitalisation rules.
            Text("settings", color = Fg, fontSize = 20.sp, fontWeight = FontWeight.SemiBold)
        }
        Column(
            Modifier
                .weight(1f)
                .fillMaxWidth()
                .verticalScroll(rememberScrollState())
                .padding(horizontal = LocalTouch.current.rowPad * 3),
            verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad * 2),
        ) {
            // The order lives in [settingsGroups], not here: it was `host`, `link`,
            // `display`, so a person reading top-down met the machine before the choice of
            // how to reach it — and then found the mesh route's pairing in a different
            // section from the tailnet one. The route is the first question and pairing is
            // the second, and the two pairings are the same kind of thing.
            Spacer(Modifier.height(8.dp))
            val groups = settingsGroups(hasMesh = mesh != null)
            for (group in groups) {
                Section(group.title) {
                    when (group) {
                        SettingsGroup.Link -> LinkChoice(current, onSave)
                        SettingsGroup.Pairing -> {
                            // **One pairing row, because there is one pairing.** A scanned code
                            // carries the daemon's address and token *and* the radio's keys, and
                            // `ConfigStore.pair` has always applied whichever halves it holds. Two
                            // rows were two buttons over that one operation, and a person had to
                            // guess which one to scan under.
                            PairingRow(
                                current = current,
                                radioPaired = radioPaired,
                                meshPresent = meshPresent,
                                problem = pairProblem,
                                note = pairNote,
                                onScan = {
                                    pairProblem = null
                                    pairNote = null
                                    CodeScanner.scan { result ->
                                        when (val o = scanOutcome(result)) {
                                            is PairOutcome.Paired -> acceptPairing(o.pairing)
                                            is PairOutcome.Problem -> pairProblem = o.text
                                            PairOutcome.Nothing -> Unit
                                        }
                                    }
                                },
                                onPaste = {
                                    pairProblem = null
                                    pasteOpen = true
                                },
                                onUnpair = {
                                    pairProblem = null
                                    pairNote = null
                                    // A radio-only install has no tailnet pairing to forget; the
                                    // mesh route owns its own keys, so the press goes there.
                                    if (current.isPaired) onUnpair() else mesh?.onUnpair()
                                    credentialsVersion++
                                },
                                onUseRadio = {
                                    mesh?.onConfig(current.copy(route = MeshRoute.Mesh))
                                    pairNote = null
                                },
                            )
                            // The radio, its preset and its credentials are one block, and it is
                            // the same block the dialog had: it decides nothing itself.
                            if (mesh != null) {
                                MeshRouteSection(
                                    current = current,
                                    credentialsVersion = credentialsVersion,
                                    readCredentials = mesh.credentials,
                                    onMeshPairing = mesh.onPairing,
                                    onConfig = mesh.onConfig,
                                    // The same `onSave` the other rows use: the config this screen
                                    // is drawing is recomposed with the new value, so the next row
                                    // that saves starts from it.
                                    onRadioChannel = { public ->
                                        onSave(current.copy(meshPublicChannel = public))
                                    },
                                    onInputMode = { mode ->
                                        onSave(current.copy(meshInputMode = mode))
                                    },
                                )
                            }
                        }
                        SettingsGroup.Display -> {
                            PanelSide(current, onSave)
                            ControlSize(current, onSave)
                            TextSize(current, onSave)
                        }
                    }
                }
                // A gap between sections and none within one: the spacing is what says
                // "this block is a different kind of thing". It was one row-pad (6 dp), which
                // is the same gap as between two rows inside a section, so the only thing
                // separating "pairing" from "display" was a heading. Twenty-four is a break
                // the eye can see without a heavier rule.
                if (group != groups.last()) {
                    Spacer(Modifier.height(SettingsSectionGap))
                }
            }
            About()
            Spacer(Modifier.height(SettingsSectionGap))
        }
    }
    // **A sibling of the screen, not a child of it.** `ModalCard` is a full-screen overlay; a
    // fill-the-parent child inside a scrolling `Column` measures against an unbounded height, so
    // it would size itself to its content and be laid out in the list instead of over the screen.
    if (pasteOpen) {
        PastePairingLink(
            onDismiss = { pasteOpen = false },
            onPair = { p ->
                pasteOpen = false
                acceptPairing(p)
            },
        )
    }
}

/**
 * The screen's sections, in the order they are read.
 *
 * A named list rather than three `if` branches in the body, so the order is one value
 * that a test can hold and a later edit cannot quietly invert. `link` is absent where the
 * platform has no second route: a choice of one is not a choice, and iOS has only the
 * tailnet.
 */
internal enum class SettingsGroup(val title: String) {
    /** Which link the app runs on. First, because it decides whether the rest applies. */
    Link("link"),
    /** Both pairings — the tailnet's and the radio's — under one heading. */
    Pairing("pairing"),
    Display("display"),
}

internal fun settingsGroups(hasMesh: Boolean): List<SettingsGroup> =
    if (hasMesh) listOf(SettingsGroup.Link, SettingsGroup.Pairing, SettingsGroup.Display)
    else listOf(SettingsGroup.Pairing, SettingsGroup.Display)

/**
 * A titled block: the heading, a rule, then its rows.
 *
 * The rule is what makes a settings screen read as a menu rather than as a paragraph. Without it
 * the headings were the same size and colour as the labels under them, so `link` and `route` read
 * as two rows of the same list instead of a heading and its first row.
 */
@Composable
private fun Section(title: String, content: @Composable androidx.compose.foundation.layout.ColumnScope.() -> Unit) {
    SettingsSection(title, content)
}

/**
 * Which link the app runs on: the tailnet, or the phone's own radio.
 *
 * Two buttons rather than a switch, because there are two named things and a switch cannot say
 * either of their names. Choosing `radio` does not start the radio - the mesh screen does that
 * when it opens - and it does not touch the tailnet pairing, which stays valid in case the user
 * switches back.
 */
@Composable
private fun LinkChoice(current: LdmuxConfig, onSave: (LdmuxConfig) -> Unit) {
    // The consequence is the row's summary rather than a line under it: it is the value's
    // explanation, and a settings row states its value beside its title.
    SettingsChoice("route", summary = routeConsequence(current.route)) {
        OptionButton("tailnet", selected = current.route != MeshRoute.Mesh) {
            if (current.route != MeshRoute.Tailnet) onSave(current.copy(route = MeshRoute.Tailnet))
        }
        // `radio`, not `lora`: the word in the row's own consequence line and in every
        // message the mesh route prints is "radio", and `lora` is the name of a modulation
        // the user never chose.
        OptionButton("LoRa radio", selected = current.route == MeshRoute.Mesh) {
            if (current.route != MeshRoute.Mesh) onSave(current.copy(route = MeshRoute.Mesh))
        }
    }
}

/**
 * What choosing a link means, in the row that chooses it.
 *
 * The consequence is worth a line because the two routes are not two settings: one is a
 * machine on a tailnet and the other is a radio in the room, and which one is in use
 * changes what "paired" even refers to.
 */
internal fun routeConsequence(route: MeshRoute): String =
    if (route == MeshRoute.Mesh) "the phone's LoRa radio" else "over Tailscale"


/**
 * One row that names a setting, then the options that set it.
 *
 * The label says what is being chosen and the option buttons carry the chosen value, so
 * no button has to be both a noun and a statement. Every row in this screen has this
 * shape; the three that did not (a bare pair with no label, a pair whose order did not
 * match the screen, and a label at a different size from its neighbours) read as three
 * different kinds of control.
 */
@Composable
private fun SettingRow(
    label: String,
    content: @Composable androidx.compose.foundation.layout.RowScope.() -> Unit,
) {
    SettingsInline(label, content = content)
}

@Composable
private fun PanelSide(current: LdmuxConfig, onSave: (LdmuxConfig) -> Unit) {
    SettingRow("panel") {
        // Left before right, because the options sit where the thing they describe sits.
        // This pair was ordered right, left: a small lie under a thumb.
        OptionButton("left", selected = current.panelLeft) {
            if (!current.panelLeft) onSave(current.copy(side = "left"))
        }
        OptionButton("right", selected = !current.panelLeft) {
            if (current.panelLeft) onSave(current.copy(side = "right"))
        }
    }
}

@Composable
private fun ControlSize(current: LdmuxConfig, onSave: (LdmuxConfig) -> Unit) {
    SettingRow("controls") {
        // `compact` and `comfortable`, not `thin` and `tall`: the setting changes the
        // height of every control, and "tall"/"thin" describe a shape rather than a
        // trade the user is making. The value is the same [LdmuxConfig.compact] either way.
        OptionButton("compact", selected = current.compact) {
            if (!current.compact) onSave(current.copy(compact = true))
        }
        OptionButton("comfortable", selected = !current.compact) {
            if (current.compact) onSave(current.copy(compact = false))
        }
    }
}

@Composable
private fun TextSize(current: LdmuxConfig, onSave: (LdmuxConfig) -> Unit) {
    SettingsRow("text size") {
        // Disabled at the ends of the range rather than silently doing nothing: a live button
        // that produces no visible change looks broken and hides the reason.
        val smaller = current.fontSp > LdmuxConfig.MIN_FONT_SP
        val larger = current.fontSp < LdmuxConfig.MAX_FONT_SP
        MiniTextButton(
            onClick = { onSave(current.copy(fontSp = (current.fontSp - 1f).coerceAtLeast(LdmuxConfig.MIN_FONT_SP))) },
            enabled = smaller,
        ) {
            Text("\u2212", color = if (smaller) Accent else Dim.copy(alpha = 0.4f), fontSize = 16.sp)
        }
        Text("${current.fontSp.toInt()}", color = Fg, fontFamily = MonoFont, fontSize = 12.sp)
        MiniTextButton(
            onClick = { onSave(current.copy(fontSp = (current.fontSp + 1f).coerceAtMost(LdmuxConfig.MAX_FONT_SP))) },
            enabled = larger,
        ) {
            Text("+", color = if (larger) Accent else Dim.copy(alpha = 0.4f), fontSize = 16.sp)
        }
    }
}

/**
 * What the pairing row says it is paired with.
 *
 * Pure, because the row has four states and only two of them are `paired`/`unpaired`: a
 * mesh-only install is paired with a radio and *not* with a daemon, and reading that as
 * "unpaired" is what made such an install impossible to unpair.
 */
internal fun pairingRowSummary(current: LdmuxConfig, radioPaired: Boolean, meshPresent: Boolean): String = when {
    current.isPaired && !meshPresent -> current.pairedName
    current.isPaired && radioPaired -> "${current.pairedName} · radio"
    current.isPaired -> "${current.pairedName} · radio not paired"
    radioPaired -> "radio only"
    else -> "unpaired"
}

/**
 * The pairing, as one row labelled next to the mesh one (ADR-0017).
 *
 * The `tailnet` label is what makes the two pairings legible as two: the mesh block below
 * has always had a heading of its own, and this row had none, so the pairing that existed
 * first was the one you had to infer.
 *
 * Unpaired: `unpaired` and the two ways to pair — `pair` opens the scanner, `paste` a link
 * for a device whose scanner cannot run. Paired: the machine's name and `unpair`, which asks
 * once (`unpair?`) because it ends every shell's link. **And it says what else it takes with
 * it**: `Link.unpair()` also forgets the mesh keys and the radio, which is not something a
 * button under `tailnet` can be expected to imply.
 *
 * **A radio-only install is paired.** `ldmux code --mesh-only` stores the radio's keys and no
 * daemon address, so `isPaired` is false; the row used to offer only `pair` and there was no
 * way to forget a radio. The verb and the summary both follow [canUnpair], not `isPaired`.
 *
 * It holds no state and opens nothing: the scan, the paste field's dialog and the outcome
 * note all belong to the screen, because a `ModalCard` has to be emitted beside the screen
 * rather than inside its scrolling column.
 */
@Composable
private fun PairingRow(
    current: LdmuxConfig,
    radioPaired: Boolean,
    meshPresent: Boolean,
    /** Why the last scan did nothing, when it did nothing. */
    problem: String?,
    /** What the last pairing did, when the screen would not otherwise show it. */
    note: PairingNote?,
    onScan: () -> Unit,
    onPaste: () -> Unit,
    /** Forget the tailnet pairing (and, with it, the mesh keys). */
    onUnpair: () -> Unit,
    /** The one press that finishes a pairing that came with no daemon address. */
    onUseRadio: () -> Unit,
) {
    // Asked once, because unpairing ends every open shell's link. Reset by any pairing change,
    // so a confirmation does not outlive the state it was asking about.
    var confirmUnpair by remember(current.connectionKey, radioPaired) { mutableStateOf(false) }
    Column {
        SettingsRow(
            // The pairing is with the desktop, and it arrives as one code whatever it carries.
            "pairing",
            summary = pairingRowSummary(current, radioPaired, meshPresent),
            trailing = {
                if (canUnpair(current.isPaired, radioPaired)) {
                    MiniTextButton(onClick = {
                        if (confirmUnpair) onUnpair() else confirmUnpair = true
                    }) {
                        Text(if (confirmUnpair) "unpair?" else "unpair", color = if (confirmUnpair) Danger else Dim)
                    }
                } else {
                    MiniTextButton(onClick = onScan) {
                        Text("pair", color = Accent)
                    }
                    MiniTextButton(onClick = onPaste) {
                        Text("paste", color = Accent)
                    }
                }
            },
        )
        // Only while the warning can be true: an install with no mesh route has nothing
        // else to lose, and a line that describes a consequence that does not exist is
        // noise on the one screen where the consequence is real.
        if (confirmUnpair && (meshPresent || radioPaired)) {
            SettingsNote(
                if (current.isPaired) "this also forgets the mesh keys and the LoRa radio"
                else "this forgets the radio keys and the chosen radio",
            )
        }
        problem?.let { SettingsNote(it, color = Danger) }
        note?.let {
            SettingsNote(it.text, color = Muted)
            if (it.useRadio) {
                MiniTextButton(onClick = onUseRadio) {
                    Text("use the radio", color = Accent, fontSize = 12.sp)
                }
            }
        }
    }
}

/**
 * The footer: which build this is.
 *
 * Centred, because it is not a setting and must not read as one — a row is left-aligned
 * because its label is a thing to scan; this is an aside about the app, and the one line a
 * support question needs. It carried a link to the project site, which answered HTTP 410. The version comes from the package, so it cannot drift from what a
 * store listing says. No copyright line: a claim about ownership answers nothing a reader of
 * a settings screen asked, and the licence files are where that question belongs.
 */
@Composable
private fun About() {
    Column(
        Modifier.fillMaxWidth().padding(top = SettingsSectionGap),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Text(
            "ldmux " + appVersionName().ifEmpty { "dev" },
            color = Dim,
            fontSize = 11.sp,
            fontFamily = MonoFont,
        )
        // **No link to the project site.** It pointed at `https://ldmux.io`, which answers
        // HTTP 410 (the domain is parked), so the one "read more" affordance in the app was a
        // dead end — and a support question cannot be answered by a URL that does not resolve.
        // When there is a page, it goes here; a version number is the fact a support question
        // actually needs.
    }
}
