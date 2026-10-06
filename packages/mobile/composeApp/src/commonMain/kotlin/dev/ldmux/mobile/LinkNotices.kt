package dev.ldmux.mobile

import androidx.compose.foundation.layout.BoxScope
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.width
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.input.pointer.pointerInput
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** A one-line notice over the top of a page: the link is the reason nothing is happening. */
@Composable
internal fun BoxScope.LinkNotice(text: String) {
    Box(
        Modifier
            .align(Alignment.TopCenter)
            .padding(8.dp)
            .clip(RadiusMedium)
            .background(Surface.copy(alpha = 0.92f))
            .border(1.dp, Line, RadiusMedium)
            .padding(horizontal = 10.dp, vertical = LocalTouch.current.rowPad),
    ) {
        Text(text, color = Warn, fontFamily = FontFamily.Monospace, fontSize = 11.sp)
    }
}

/**
 * "Another device is using this shell."
 *
 * Opening a shell takes it, so this is only seen when somebody took it back while
 * it was open here — the desk pressing `r` in its window, or another phone. One
 * tap takes it again: the tap is as deliberate as opening it was.
 */
@Composable
internal fun BoxScope.HeldElsewhere(holder: String, onTakeOver: () -> Unit) {
    Box(
        Modifier
            .fillMaxSize()
            // Consume every pointer event. An overlay that only *looks* modal is
            // worse than none: a tap fell through to the terminal underneath and
            // moved its cursor, and a drag fell through to the key bars, all in a
            // shell this device does not hold and whose input the daemon refuses.
            // The one control below is the only thing that should react.
            .pointerInput(Unit) {
                awaitPointerEventScope {
                    while (true) {
                        awaitPointerEvent().changes.forEach { it.consume() }
                    }
                }
            }
            .background(Bg.copy(alpha = 0.96f)),
        contentAlignment = Alignment.Center,
    ) {
        Column(horizontalAlignment = Alignment.CenterHorizontally) {
            Text("open on $holder", color = Fg, fontSize = 15.sp)
            Spacer(Modifier.height(LocalTouch.current.rowPad * 2))
            AccentBtn("take it over", Modifier.width(220.dp)) { onTakeOver() }
        }
    }
}
