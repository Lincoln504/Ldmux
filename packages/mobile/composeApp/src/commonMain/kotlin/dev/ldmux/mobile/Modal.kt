package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.BasicTextField
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.TextStyle
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * A dialogue, drawn by hand.
 *
 * Material's own dialog arrives in Material's light scheme and renders as a white
 * rectangle in this app — the same failure the overflow menu had. More to the
 * point, this app is one palette and a dialog is the place it would show most: a
 * scrim, a card, and the app's own type.
 */
@Composable
fun ModalCard(title: String, onDismiss: () -> Unit, content: @Composable ColumnScope.() -> Unit) {
    Box(
        Modifier.fillMaxSize().background(Bg.copy(alpha = 0.72f)).clickable(onClick = onDismiss),
        contentAlignment = Alignment.Center,
    ) {
        Column(
            Modifier
                .fillMaxWidth(0.92f)
                .clip(RadiusLarge)
                .background(Surface)
                // The click below stops a tap inside the card from reaching the
                // scrim: a dialogue you dismiss by touching its own contents is a
                // dialogue that cannot be filled in.
                .clickable(enabled = false, onClick = {})
                .padding(LocalTouch.current.rowPad * 2),
            verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad * 2),
        ) {
            Text(title, color = Fg, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
            content()
        }
    }
}

/**
 * One line of text, in the app's own colours.
 *
 * A monospace field, because everything typed here is a path or a command and the
 * one thing both need is that the characters line up with what is on the host.
 */
@Composable
private fun Field(
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
) {
    FieldBox(placeholder, value.isEmpty(), modifier) {
        BasicTextField(
            value = value,
            onValueChange = onValueChange,
            singleLine = true,
            textStyle = FieldText,
            cursorBrush = SolidColor(Accent),
            modifier = Modifier.fillMaxWidth(),
        )
    }
}

/**
 * The same field, holding a value that includes where the caret is.
 *
 * A plain `String` field cannot say where the caret should be, and the new-shell field
 * has to: it opens holding `~/` with the caret after it, so the first character typed
 * continues a path rather than landing in front of the tilde. Compose's `String`
 * overload keeps its own caret and does not offer to move it, so the one field that
 * needs to say gets to say.
 */
@Composable
private fun FieldValue(
    value: TextFieldValue,
    onValueChange: (TextFieldValue) -> Unit,
    placeholder: String,
    modifier: Modifier = Modifier,
    focusRequester: FocusRequester? = null,
) {
    FieldBox(placeholder, value.text.isEmpty(), modifier) {
        BasicTextField(
            value = value,
            onValueChange = onValueChange,
            singleLine = true,
            textStyle = FieldText,
            cursorBrush = SolidColor(Accent),
            // The caret is only real if the field has focus, and a field that opens
            // unfocused shows a caret position that typing does not go to. Measured:
            // with the value prefilled and nothing requesting focus, the field read
            // `~/` and stayed `~/` through both `input text tmp` and two deletes.
            modifier = Modifier.fillMaxWidth().then(
                if (focusRequester != null) Modifier.focusRequester(focusRequester) else Modifier,
            ),
        )
    }
}

/**
 * The box a field is drawn in, shared by both value types.
 *
 * Shared rather than written twice: two fields that are meant to look identical and are
 * drawn in two places are two fields that stop looking identical.
 */
@Composable
private fun FieldBox(placeholder: String, empty: Boolean, modifier: Modifier = Modifier, content: @Composable () -> Unit) {
    Box(
        modifier
            .fillMaxWidth()
            .clip(RadiusMedium)
            .background(Bg)
            .border(1.dp, Line, RadiusMedium)
            .padding(horizontal = 10.dp, vertical = LocalTouch.current.pad),
    ) {
        if (empty) {
            Text(placeholder, color = Dim.copy(alpha = 0.6f), fontFamily = FontFamily.Monospace, fontSize = 13.sp)
        }
        content()
    }
}

/** One text style for every field, so the fields line up with each other. */
private val FieldText = TextStyle(color = Fg, fontFamily = FontFamily.Monospace, fontSize = 13.sp)

/** A labelled field, because "which box is the path" is not a question to ask. */
@Composable
private fun LabelledField(
    label: String,
    value: String,
    onValueChange: (String) -> Unit,
    placeholder: String,
) {
    Column(verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad)) {
        Text(label, color = Dim, fontSize = 10.sp)
        Field(value, onValueChange, placeholder)
    }
}

/**
 * A new shell, asked for one way: where.
 *
 * What runs there is not a choice — it is always the host's login shell
 * (docs/SPEC.md §7), the same as `ldmux shell` and everything else that opens
 * one. A command field here was a second way to start a shell that looked like
 * a way to *choose the program*, and the daemon does not offer that: every
 * shell is the login shell, full stop. Only the directory is worth asking for,
 * because a shell in the wrong one is not obviously wrong.
 */
@Composable
fun NewShellModal(
    onDismiss: () -> Unit,
    onCreate: (path: String) -> Unit,
) {
    // It opens holding `~/` with the caret after it, and that is the whole design. The
    // tilde is text the user can delete rather than a decoration: it is a real path the
    // daemon understands, so leaving it alone means home and typing after it means a
    // path inside home. Nothing is prefilled from the launcher's selection any more --
    // a field that opens holding a path nobody typed, with nothing saying where it came
    // from, is a field that gets appended to by mistake, which is exactly what happened.
    var path by remember { mutableStateOf(TextFieldValue("~/", TextRange(2))) }
    // Ask for focus once, when the modal appears, so the caret is live and the keyboard
    // is up: the whole point of the prefill is that the next keystroke lands after it.
    val focus = remember { FocusRequester() }
    LaunchedEffect(Unit) { focus.requestFocus() }

    ModalCard("new shell", onDismiss) {
        Column(verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad)) {
            Text("path", color = Dim, fontSize = 10.sp)
            FieldValue(path, { path = it }, "~", focusRequester = focus)
        }
        Text(
            "Opens the host's login shell there. Empty means your home directory.",
            color = Dim.copy(alpha = 0.7f),
            fontSize = 10.sp,
        )
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            OutlinedBtn("cancel", Modifier.weight(1f)) { onDismiss() }
            AccentBtn("create", Modifier.weight(1f)) { onCreate(path.text.trim()) }
        }
    }
}

/**
 * Choosing a desktop to put a shell on.
 *
 * Desktops **with windows on them**, and nothing else: a workspace you cannot see
 * is a workspace where a window goes missing, and "1 through 10" would offer nine
 * places nobody is looking at.
 *
 * Each one carries its windows as a parenthesised line under it, in ordinary
 * light text rather than as a title — that is enough to tell "the one with the
 * editor" from "the one with the browser", which is the whole question being
 * asked, and a list of window rows would be a list of things that are *not* the
 * choice.
 */
@Composable
fun DesktopPickerModal(
    workspaces: List<DesktopWorkspace>,
    windows: List<DesktopWindow>,
    liveOn: String?,
    onDismiss: () -> Unit,
    onPick: (String) -> Unit,
    onTakeOff: () -> Unit,
) {
    // Derived from the windows rather than from `windowCount`: the count is the
    // WM's number and the titles are what the user is choosing between, so the
    // list has to be able to show them.
    val occupied = workspaces.filter { ws -> windows.any { it.workspace == ws.name } }

    ModalCard("send to desktop", onDismiss) {
        if (occupied.isEmpty()) {
            Text("No desktop has a window on it.", color = Dim, fontSize = 12.sp)
        } else {
            Column(
                Modifier.heightIn(max = 320.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad),
            ) {
                for (ws in occupied) {
                    val titles = windows.filter { it.workspace == ws.name }
                        .joinToString(", ") { it.title.ifEmpty { it.appClass } }
                    val here = liveOn == ws.name
                    Column(
                        Modifier
                            .fillMaxWidth()
                            .clip(RadiusMedium)
                            .background(if (here) Bg else Surface)
                            .clickable { onPick(ws.name) }
                            .padding(horizontal = 10.dp, vertical = LocalTouch.current.rowPad),
                        verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad / 2),
                    ) {
                        Text(
                            ws.name,
                            color = if (here) Accent else Fg,
                            fontSize = 14.sp,
                            fontWeight = if (here) FontWeight.SemiBold else FontWeight.Normal,
                        )
                        // Ordinary size, light: a subtitle, not a heading. Wrapped
                        // rather than one long line, because window titles are long.
                        Text("($titles)", color = Dim, fontSize = 12.sp)
                    }
                }
            }
        }
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            if (liveOn != null) {
                OutlinedBtn("take it off ${liveOn}", Modifier.weight(1f)) { onTakeOff() }
            }
            OutlinedBtn("cancel", Modifier.weight(1f)) { onDismiss() }
        }
    }
}
