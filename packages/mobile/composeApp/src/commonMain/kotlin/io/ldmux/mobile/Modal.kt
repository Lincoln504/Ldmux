package io.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.gestures.detectTapGestures
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
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
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.draw.clip
import androidx.compose.ui.focus.FocusRequester
import androidx.compose.ui.focus.focusRequester
import androidx.compose.ui.graphics.SolidColor
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.TextStyle
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
    // **The system back gesture dismisses this card.** It was the one thing Material's
    // own dialog gave for free and this hand-drawn one did not: with a modal open on the
    // launcher, back reached the activity and left the app with the user's half-typed
    // path in a field nobody would see again. The card is the topmost thing on screen,
    // so it is what back must close, and the caller's `onDismiss` is already the same
    // action as tapping the scrim.
    BackToLauncher(enabled = true) { onDismiss() }
    Box(
        Modifier
            .fillMaxSize()
            .background(Bg.copy(alpha = 0.72f))
            // **A scrim that is a tap target, not a clickable.** `clickable` puts an unlabelled
            // full-screen button in the semantics tree and merges every descendant into it, so a
            // screen reader read the scrim instead of the dialog. Dismissing by tapping outside is
            // a pointer convenience; the card's own buttons and the back gesture are the ways out
            // that a reader can reach.
            .pointerInput(Unit) { detectTapGestures(onTap = { onDismiss() }) },
        contentAlignment = Alignment.Center,
    ) {
        Column(
            Modifier
                .fillMaxWidth(0.92f)
                .clip(RadiusLarge)
                .background(Surface)
                // **Consumed, not disabled.** `clickable(enabled = false)` does not take the event:
                // a disabled node does not consume, so the tap fell through to the scrim and a
                // touch on the card's own title or padding dismissed the dialogue, losing whatever
                // was half-typed in it. This is the consumer `HeldElsewhere` already uses for the
                // same reason.
                .pointerInput(Unit) {
                    awaitPointerEventScope {
                        while (true) {
                            awaitPointerEvent().changes.forEach { it.consume() }
                        }
                    }
                }
                .padding(LocalTouch.current.rowPad * 2),
            verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad * 2),
        ) {
            Text(title, color = Fg, fontSize = 16.sp, fontWeight = FontWeight.SemiBold)
            content()
        }
    }
}

/**
 * The row of buttons at the foot of a modal.
 *
 * One composable so every modal puts its actions in the same place, at the same
 * spacing, in the same order — cancel first, the action that changes something last.
 * Four dialogs each drawing their own row is how one of them ends up with its primary
 * button on the left.
 */
@Composable
fun ModalActions(content: @Composable RowScope.() -> Unit) {
    Row(
        Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
        content = content,
    )
}

/**
 * One line of text, in the app's own colours.
 *
 * A monospace field, because everything typed here is a path, a command or a radio tuple and the
 * one thing all three need is that the characters line up with what is on the host. Public because
 * the radio dialog's custom tuple is the second caller, and a second hand-drawn field would be a
 * second look for the same thing.
 */
@Composable
fun Field(
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
            .border(1.dp, Edge, RadiusMedium)
            .padding(horizontal = 10.dp, vertical = LocalTouch.current.pad),
    ) {
        if (empty) {
            Text(placeholder, color = Muted, fontFamily = MonoFont, fontSize = 13.sp)
        }
        content()
    }
}

/** One text style for every field, so the fields line up with each other. */
private val FieldText = TextStyle(color = Fg, fontFamily = MonoFont, fontSize = 13.sp)

/**
 * A new shell, asked for one way: where.
 *
 * What runs there is not a choice — it is always the host's login shell
 * the same as `ldmux shell` and everything else that opens
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
            "Runs the host's login shell there · empty means home directory",
            color = Muted,
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
 * The workspaces worth offering: the ones with windows on them, **and the one the
 * user is looking at even when it is empty**. Leaving the focused workspace out was
 * the bug this exists to fix: the only place a person wants a window is wherever
 * they are, a fresh workspace has nothing on it yet, and the list refused to name
 * it — so "send to desktop" offered a list of somewhere else, or nothing at all.
 *
 * Each one carries its windows as a parenthesised line under it, in ordinary
 * light text rather than as a title — that is enough to tell "the one with the
 * editor" from "the one with the browser", which is the whole question being
 * asked, and a list of window rows would be a list of things that are *not* the
 * choice.
 */
internal fun choosableWorkspaces(
    workspaces: List<DesktopWorkspace>,
    windows: List<DesktopWindow>,
): List<DesktopWorkspace> =
    workspaces.filter { ws ->
        ws.windowCount > 0 || ws.focused || ws.visible || windows.any { it.workspace == ws.name }
    }

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
    val choosable = choosableWorkspaces(workspaces, windows)

    ModalCard("send to desktop", onDismiss) {
        if (choosable.isEmpty()) {
            Text("no desktop available", color = Muted, fontSize = 12.sp)
        } else {
            Column(
                Modifier.heightIn(max = 320.dp).verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(LocalTouch.current.rowPad),
            ) {
                for (ws in choosable) {
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
                        // An empty workspace says so, rather than showing a bare `()`.
                        Text(
                            if (titles.isEmpty()) "(empty)" else "($titles)",
                            color = Dim,
                            fontSize = 12.sp,
                        )
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
