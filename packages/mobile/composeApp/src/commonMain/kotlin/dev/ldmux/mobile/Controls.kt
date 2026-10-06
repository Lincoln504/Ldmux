package dev.ldmux.mobile

import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.ui.unit.sp

/** A button that carries the accent, for the single primary action. */
@Composable
fun AccentBtn(label: String, modifier: Modifier = Modifier, onClick: () -> Unit) {
    Box(
        modifier
            .clip(RadiusMedium)
            .background(Accent)
            .heightIn(min = LocalTouch.current.min)
            .clickable(role = Role.Button, onClick = onClick)
            .padding(horizontal = 10.dp, vertical = LocalTouch.current.pad),
        contentAlignment = Alignment.Center,
    ) {
        Text(label, color = Bg, fontSize = 13.sp)
    }
}

/**
 * A Material text button at this app's own control height.
 *
 * Material's buttons carry a minimum height of their own and no `CompositionLocal`
 * can reach it, so every `TextButton` in the app was 48dp in both densities. That is
 * why the settings dialog -- the one screen a person opens to check the density
 * setting -- was the one screen the setting could not thin. `Modifier.height`
 * overrides Material's `defaultMinSize`, which is what set the 48dp.
 *
 * One wrapper rather than a modifier repeated at eleven call sites, because eleven
 * call sites is how a setting ends up reaching most of the controls and not all of
 * them, which is the failure this app already had once.
 */
@Composable
internal fun MiniTextButton(
    onClick: () -> Unit,
    enabled: Boolean = true,
    content: @Composable RowScope.() -> Unit,
) {
    TextButton(
        onClick = onClick,
        enabled = enabled,
        modifier = Modifier.height(LocalTouch.current.min),
        contentPadding = PaddingValues(horizontal = 8.dp, vertical = 0.dp),
        content = content,
    )
}
