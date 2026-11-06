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
import io.ldmux.mobile.mesh.MeshRouteActions
import io.ldmux.mobile.mesh.MeshRouteSection
import io.ldmux.mobile.mesh.MeshRoute

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
                            PairingRow(current, onPaired, onUnpair, mesh = mesh)
                            // The radio, its preset and its credentials are one block, and it is
                            // the same block the dialog had: it decides nothing itself.
                            if (mesh != null) {
                                MeshRouteSection(
                                    current = current,
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
 * The tailnet pairing, as one row labelled next to the mesh one (ADR-0017).
 *
 * The `tailnet` label is what makes the two pairings legible as two: the mesh block below
 * has always had a heading of its own, and this row had none, so the pairing that existed
 * first was the one you had to infer.
 *
 * Unpaired: `unpaired` and `pair`, which opens the scanner. Paired: the machine's name and
 * `unpair`, which asks once (`unpair?`) because it ends every shell's link. **And it says
 * what else it takes with it**: `Link.unpair()` also forgets the mesh keys and the radio,
 * which is not something a button under `tailnet` can be expected to imply.
 *
 * A scan that is not an ldmux code says so on the row, in three words.
 */
@Composable
private fun PairingRow(
    current: LdmuxConfig,
    onPaired: (Pairing) -> Unit,
    onUnpair: () -> Unit,
    /** The radio route, when this install has one: its keys arrive in the same code. */
    mesh: MeshRouteActions?,
) {
    val meshPresent = mesh != null
    // What the code stored for the radio, read from secure storage rather than remembered here.
    var radioPaired by remember(mesh, current.connectionKey) {
        mutableStateOf(mesh?.credentials()?.complete == true)
    }
    var problem by remember { mutableStateOf<String?>(null) }
    var confirmUnpair by remember(current.connectionKey) { mutableStateOf(false) }
    Column {
        SettingsRow(
            // The pairing is with the desktop, and it arrives as one code whatever it carries.
            "pairing",
            summary = when {
                current.isPaired && !meshPresent -> current.pairedName
                current.isPaired && radioPaired -> "${current.pairedName} · radio"
                current.isPaired -> "${current.pairedName} · radio not paired"
                radioPaired -> "radio only"
                else -> "unpaired"
            },
            trailing = {
            if (current.isPaired) {
                MiniTextButton(onClick = {
                    if (confirmUnpair) onUnpair() else confirmUnpair = true
                }) {
                    Text(if (confirmUnpair) "unpair?" else "unpair", color = if (confirmUnpair) Danger else Dim)
                }
            } else {
                MiniTextButton(onClick = {
                    problem = null
                    CodeScanner.scan { result ->
                        when (val o = scanOutcome(result)) {
                            is PairOutcome.Paired -> {
                                onPaired(o.pairing)
                                radioPaired = mesh?.credentials()?.complete == true
                            }
                            is PairOutcome.Problem -> problem = o.text
                            PairOutcome.Nothing -> Unit
                        }
                    }
                }) {
                    Text("pair", color = Accent)
                }
            }
            },
        )
        // Only while the warning can be true: an install with no mesh route has nothing
        // else to lose, and a line that describes a consequence that does not exist is
        // noise on the one screen where the consequence is real.
        if (confirmUnpair && meshPresent) {
            SettingsNote("this also forgets the mesh keys and the LoRa radio")
        }
        problem?.let { SettingsNote(it, color = Danger) }
    }
}

/**
 * The footer: which build this is, and where the project lives.
 *
 * Centred, because it is not a setting and must not read as one — a row is left-aligned
 * because its label is a thing to scan; this is an aside about the app, and the one line a
 * support question needs. The version comes from the package, so it cannot drift from what a
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
        Spacer(Modifier.height(2.dp))
        MiniTextButton(onClick = { openUrl("https://ldmux.io") }, bordered = false) {
            Text("ldmux.io", color = Accent, fontSize = 12.sp)
        }
    }
}
