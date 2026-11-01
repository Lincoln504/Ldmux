package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/**
 * The app's header: the name, the one line that says which link this is and how it is
 * doing, and the two square buttons every screen has.
 *
 * **It is one header, not one per route.** The mesh route drew its own row — a status
 * text and `leave` — so the mesh terminal looked like a different app that had taken the
 * screen over: no refresh, and no way into Settings except leaving first, which then said
 * `mesh route unavailable`. Every route draws this, so the chrome is the app and only the
 * body differs.
 *
 * [onRefresh] is null where a refresh would mean nothing; the button is then absent rather
 * than present and inert.
 *
 * [routes] is the way between the tailnet route and the mesh route, and it lives **here**, in
 * the app's own header, as a dropdown: it is a choice about the link, not a setting about the
 * terminal, and it must not be buried in Settings. The mesh route has no separate exit either
 * (no `leave`): the same header switches, on the same screen, in the same app.
 */
@Composable
internal fun AppHeader(
    title: String,
    onRefresh: (() -> Unit)?,
    /** A refresh in flight: the arrow dims rather than turning into text. */
    refreshing: Boolean = false,
    onSettings: () -> Unit,
    /** Selectable routes, when this build has more than one. Empty hides the control. */
    routes: List<RouteChoice> = emptyList(),
    currentRoute: RouteChoice? = null,
    onRoute: (RouteChoice) -> Unit = {},
) {
    Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        Column(Modifier.weight(1f)) {
            // One line, and nothing under it. A second line under the name held the route's
            // status - "meshmore · securing the link", the old "tailnet" - and it duplicated what
            // the body already says: a wait has its own screen, and a failure has its sentence.
            // It also held the height of a line even when empty, which is the sort of thing that
            // looks like a layout bug.
            Text(title, color = Fg, fontSize = 20.sp, fontWeight = FontWeight.SemiBold)
        }
        if (routes.size > 1 && currentRoute != null) {
            Box {
                var open by remember { mutableStateOf(false) }
                Text(
                    currentRoute.label,
                    color = Accent,
                    fontSize = 13.sp,
                    maxLines = 1,
                    modifier = Modifier
                        .padding(4.dp)
                        .clickable(role = Role.Button) { open = true },
                )
                DropdownMenu(expanded = open, onDismissRequest = { open = false }) {
                    for (route in routes) {
                        DropdownMenuItem(
                            text = { Text(if (route == currentRoute) "${route.label} ✓" else route.label, color = Fg, fontSize = 13.sp) },
                            onClick = { open = false; onRoute(route) },
                        )
                    }
                }
            }
            Spacer(Modifier.width(6.dp))
        }
        if (onRefresh != null) {
            HeaderIcon(label = if (refreshing) "refreshing" else "refresh", onClick = onRefresh) {
                // Dimmed as one layer: a translucent tint would draw darker where the
                // head overlaps the arc.
                Box(Modifier.alpha(if (refreshing) 0.4f else 1f)) { IconRefresh(Dim) }
            }
            Spacer(Modifier.width(6.dp))
        }
        HeaderIcon(label = "settings", onClick = onSettings) { IconCog(Dim) }
    }
}

/** A square header button on the list rows' background; drawn icons need a spoken [label]. */
@Composable
internal fun HeaderIcon(label: String, onClick: () -> Unit, content: @Composable () -> Unit) {
    Box(
        Modifier
            .clip(RadiusMedium)
            .background(Surface)
            .size(LocalTouch.current.min)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics { contentDescription = label },
        contentAlignment = Alignment.Center,
    ) {
        content()
    }
}

/** A route this build offers, as the header shows it: the app's own name for the link. */
data class RouteChoice(val label: String, val mesh: Boolean)
