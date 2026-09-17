package dev.ldmux.mobile

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.CornerRadius
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.geometry.Size
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.material3.Text
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.unit.sp
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.sin

/**
 * The icon set, drawn rather than typed.
 *
 * Every glyph these controls used before came out of the system's monospace
 * fallback, so the icon for "flip the screen" arrived looking like whatever the
 * device happened to have — next to a keyboard glyph, which is exactly why the
 * two were confusable. Drawing them costs a few dozen lines, looks identical on
 * every device and density, and lets the flip icon be what it is: two arrows
 * going round.
 */

/** A square icon button with an optional caption underneath. */
@Composable
fun IconBtn(
    onClick: () -> Unit,
    modifier: Modifier = Modifier,
    caption: String? = null,
    content: @Composable () -> Unit,
) {
    Column(
        modifier
            .clip(RoundedCornerShape(8.dp))
            .background(Bg)
            .clickable(onClick = onClick)
            .padding(horizontal = 9.dp, vertical = 6.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        content()
        if (caption != null) {
            Text(caption, color = Dim, fontFamily = FontFamily.Monospace, fontSize = 8.sp)
        }
    }
}

/** A one-pixel rule that separates control groups. */
@Composable
fun PanelDivider() {
    Column(Modifier.fillMaxWidth().height(1.dp).background(Color(0xFF232838))) {}
}

@Composable
fun IconFlip(tint: Color, box: Dp = 22.dp) {
    Canvas(Modifier.size(box)) { drawFlip(tint) }
}

@Composable
fun IconMic(tint: Color, box: Dp = 22.dp) {
    Canvas(Modifier.size(box)) { drawMic(tint) }
}

@Composable
fun IconClose(tint: Color, box: Dp = 18.dp) {
    Canvas(Modifier.size(box)) {
        val w = size.minDimension
        val stroke = w * 0.13f
        drawLine(tint, Offset(w * 0.22f, w * 0.22f), Offset(w * 0.78f, w * 0.78f), stroke, StrokeCap.Round)
        drawLine(tint, Offset(w * 0.78f, w * 0.22f), Offset(w * 0.22f, w * 0.78f), stroke, StrokeCap.Round)
    }
}

/** Settings: three sliders, which reads as "adjust" without a gear's detail. */
@Composable
fun IconCog(tint: Color, box: Dp = 20.dp) {
    Canvas(Modifier.size(box)) {
        val w = size.minDimension
        val stroke = w * 0.10f
        val rows = listOf(0.26f to 0.66f, 0.50f to 0.34f, 0.74f to 0.58f)
        for ((y, knob) in rows) {
            drawLine(tint, Offset(w * 0.14f, w * y), Offset(w * 0.86f, w * y), stroke, StrokeCap.Round)
            drawCircle(Bg, w * 0.14f, Offset(w * knob, w * y))
            drawCircle(tint, w * 0.14f, Offset(w * knob, w * y), style = Stroke(stroke))
        }
    }
}

/**
 * Put this on the desktop: a panel with an arrow leaving it.
 *
 * The arrow points out and up, so it reads as "send it over there" rather than
 * "download" (down) or "open" (up, no panel).
 */
@Composable
fun IconPush(tint: Color, box: Dp = 20.dp) {
    Canvas(Modifier.size(box)) {
        val w = size.minDimension
        val stroke = w * 0.11f
        // The panel: three sides, left open where the arrow leaves.
        val x0 = w * 0.16f
        val y0 = w * 0.34f
        val x1 = w * 0.62f
        val y1 = w * 0.86f
        drawLine(tint, Offset(x0, y0), Offset(x0, y1), stroke, StrokeCap.Round)
        drawLine(tint, Offset(x0, y1), Offset(x1, y1), stroke, StrokeCap.Round)
        drawLine(tint, Offset(x1, y1), Offset(x1, y0), stroke, StrokeCap.Round)
        // The arrow, leaving through the open corner.
        val ax = w * 0.88f
        val ay = w * 0.12f
        drawLine(tint, Offset(w * 0.52f, w * 0.48f), Offset(ax, ay), stroke, StrokeCap.Round)
        val head = w * 0.16f
        drawLine(tint, Offset(ax, ay), Offset(ax - head, ay), stroke, StrokeCap.Round)
        drawLine(tint, Offset(ax, ay), Offset(ax, ay + head), stroke, StrokeCap.Round)
    }
}

/**
 * The launcher: a 2×2 grid of panes.
 *
 * Four tiles, because the launcher is a *set of places* to choose between — and
 * because one tile, or three bars, would read as "menu" or "list" and this is
 * not a list of one thing.
 */
@Composable
fun IconLauncher(tint: Color, box: Dp = 20.dp) {
    Canvas(Modifier.size(box)) {
        val w = size.minDimension
        val inset = w * 0.16f
        val gap = w * 0.12f
        val side = (w - inset * 2f - gap) / 2f
        val radius = CornerRadius(side * 0.22f, side * 0.22f)
        for (x in listOf(inset, inset + side + gap)) {
            for (y in listOf(inset, inset + side + gap)) {
                drawRoundRect(tint, Offset(x, y), Size(side, side), radius)
            }
        }
    }
}

/**
 * Two arrows chasing each other round a circle.
 *
 * Drawn as two arcs with the gaps left where the arrowheads sit, so the head is
 * always tangent to the circle — the sketch of "rotate" rather than a character
 * that a font may or may not contain.
 */
private fun DrawScope.drawFlip(tint: Color) {
    val w = size.minDimension
    val stroke = w * 0.10f
    val inset = w * 0.28f
    val d = w - inset * 2f
    val tl = Offset(inset, inset)
    val arc = Size(d, d)
    val first = -35f
    val sweep = 120f
    val style = Stroke(stroke, cap = StrokeCap.Round)
    drawArc(tint, first, sweep, false, tl, arc, style = style)
    drawArc(tint, first + 180f, sweep, false, tl, arc, style = style)
    arrowHead(tint, tl, d, first + sweep, w * 0.22f)
    arrowHead(tint, tl, d, first + 180f + sweep, w * 0.22f)
}

/** A filled triangle tangent to the circle at [angleDeg], pointing clockwise. */
private fun DrawScope.arrowHead(tint: Color, tl: Offset, d: Float, angleDeg: Float, len: Float) {
    val r = d / 2f
    val cx = tl.x + r
    val cy = tl.y + r
    val a = angleDeg * (PI / 180.0)
    val co = cos(a).toFloat()
    val si = sin(a).toFloat()
    val tip = Offset(cx + r * co, cy + r * si)
    // With y growing downwards, the clockwise tangent at `a` is (-sin, cos).
    val back = Offset(tip.x + si * len, tip.y - co * len)
    val half = Offset(co * len * 0.42f, si * len * 0.42f)
    val p1 = Offset(back.x + half.x, back.y + half.y)
    val p2 = Offset(back.x - half.x, back.y - half.y)
    drawPath(
        Path().apply {
            moveTo(tip.x, tip.y)
            lineTo(p1.x, p1.y)
            lineTo(p2.x, p2.y)
            close()
        },
        tint,
    )
}

/** A keyboard: a rounded case, two rows of keys, and a space bar. */
private fun DrawScope.drawKeyboard(tint: Color) {
    val w = size.minDimension
    val stroke = w * 0.085f
    drawRoundRect(
        tint,
        topLeft = Offset(w * 0.06f, w * 0.20f),
        size = Size(w * 0.88f, w * 0.60f),
        cornerRadius = CornerRadius(w * 0.11f, w * 0.11f),
        style = Stroke(stroke),
    )
    for (row in 0..1) {
        for (col in 0..3) {
            drawCircle(tint, w * 0.045f, Offset(w * (0.22f + col * 0.185f), w * (0.36f + row * 0.16f)))
        }
    }
    drawLine(
        tint,
        Offset(w * 0.30f, w * 0.68f),
        Offset(w * 0.70f, w * 0.68f),
        stroke,
        StrokeCap.Round,
    )
}

/** A microphone: capsule, cradle and stem. */
private fun DrawScope.drawMic(tint: Color) {
    val w = size.minDimension
    val stroke = w * 0.095f
    drawRoundRect(
        tint,
        topLeft = Offset(w * 0.37f, w * 0.08f),
        size = Size(w * 0.26f, w * 0.44f),
        cornerRadius = CornerRadius(w * 0.13f, w * 0.13f),
        style = Stroke(stroke),
    )
    drawArc(
        tint,
        0f,
        180f,
        false,
        Offset(w * 0.23f, w * 0.26f),
        Size(w * 0.54f, w * 0.48f),
        style = Stroke(stroke, cap = StrokeCap.Round),
    )
    drawLine(tint, Offset(w * 0.5f, w * 0.76f), Offset(w * 0.5f, w * 0.93f), stroke, StrokeCap.Round)
}

/**
 * `/home/someone/a/b` -> `~/a/b`.
 *
 * The panel has room for one line, and the part of a path that identifies a
 * session is the part below the user's home — a truncated prefix that reads
 * `/home/ldeen/Documents/` for every session is worse than useless.
 */
fun tildify(path: String): String {
    val m = Regex("^/(home|Users)/[^/]+").find(path) ?: return path
    return "~" + path.removePrefix(m.value)
}
/**
 * The overflow menu: six dots, not three lines.
 *
 * A hamburger reads as "list", and this is not a list — it is the few things
 * that act on the app rather than on the shell, kept out of the way of the keys.
 * Dots read as "more", which is what it is.
 */
@Composable
fun IconWaffle(tint: Color, box: Dp = 20.dp) {
    Canvas(Modifier.size(box)) {
        val w = size.minDimension
        val r = w * 0.085f
        for (row in 0..2) {
            for (col in 0..1) {
                drawCircle(
                    tint,
                    r,
                    Offset(w * (0.36f + col * 0.28f), w * (0.22f + row * 0.28f)),
                )
            }
        }
    }
}
