package io.ldmux.mobile

import android.graphics.Color
import android.graphics.drawable.ColorDrawable
import androidx.activity.ComponentActivity
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import androidx.compose.ui.graphics.toArgb

/**
 * The window and its bars, in the theme's own colours.
 *
 * Two things have to follow a theme on Android and neither is a Compose concern:
 *
 *  - **The bar icons.** `enableEdgeToEdge` picks light or dark bar *icons* from the style it is
 *    given, and the app passed `SystemBarStyle.dark` once at startup (correct for the app it used to
 *    be). On a white theme that is white icons on white — the clock and the battery disappear.
 *  - **The window background.** It is drawn before the first Compose frame and in the recents
 *    thumbnail, so a white theme over the dark theme's near-black window flashes on every launch.
 *
 * Both are set here, on the UI thread, from the theme that is about to be drawn. The scrims are
 * transparent because the content draws under the bars (`safeDrawingPadding` insets it).
 */
internal actual fun applyWindowTheme(theme: LdmuxTheme) {
    val activity = LdmuxService.activity as? ComponentActivity ?: return
    val barStyle = if (theme.lightGround) {
        // `light` means "light the bars" — dark icons on a pale ground.
        SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT)
    } else {
        SystemBarStyle.dark(Color.TRANSPARENT)
    }
    activity.runOnUiThread {
        activity.enableEdgeToEdge(statusBarStyle = barStyle, navigationBarStyle = barStyle)
        activity.window?.setBackgroundDrawable(ColorDrawable(theme.palette.base.toArgb()))
    }
}
