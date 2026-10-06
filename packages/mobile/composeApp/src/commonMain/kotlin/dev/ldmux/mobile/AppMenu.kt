package dev.ldmux.mobile

import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.layout.PaddingValues

/**
 * The overflow menu: the few things that act on the *app*.
 *
 * One composable for both modes, because it is the same three actions in both and
 * a second copy would be a second place for them to drift apart. Wrapped in a
 * dark scheme because Material components arrive wearing Material's default
 * *light* theme, and a white rectangle in a dark app is what that looks like.
 */
@Composable
fun AppMenu(
    expanded: Boolean,
    onDismiss: () -> Unit,
    onLauncher: () -> Unit,
) {
    MaterialTheme(
        colorScheme = LdmuxColors,
    ) {
        DropdownMenu(
            expanded = expanded,
            onDismissRequest = onDismiss,
            // Two Material defaults have to go, and both for the same reason: this
            // app has no hue. The container is `surfaceContainer` (a purple-tinted
            // grey), and Material then paints a *tonal* elevation over it, which in
            // a dark scheme is an overlay mixed from `primary` — the menu came out
            // faintly violet against a screen that is black and grey.
            containerColor = Surface,
            tonalElevation = 0.dp,
        ) {
            DropdownMenuItem(
                text = { Text("shells", color = Fg) },
                onClick = { onDismiss(); onLauncher() },
                // Material's menu item is 48dp and no CompositionLocal reaches it,
                // which is why the app's own menu stayed thick at every density.
                modifier = Modifier.height(LocalTouch.current.min),
                contentPadding = PaddingValues(horizontal = 12.dp, vertical = 0.dp),
            )
            // Settings is a separate button on the toolbar, not part of the overflow
            // menu — keeping this a single-item dropdown keeps the menu from being a
            // full-width bar that wastes screen real estate.
        }
    }
}
