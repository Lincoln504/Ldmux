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
import kotlin.math.PI
import kotlin.math.cos
import kotlin.math.sin
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
    /**
     * Carry the accent, for a control that belongs to the app rather than to the
     * shell.
     *
     * Every other icon button is a dark square the same size as a key, which is
     * what a key looks like. A control that *leaves* this screen must not be
     * mistaken for one you type into.
     */
    filled: Boolean = false,
    content: @Composable () -> Unit,
) {
    Column(
        modifier
            .clip(RoundedCornerShape(8.dp))
            .background(if (filled) Accent else Bg)
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
    Column(Modifier.fillMaxWidth().height(1.dp).background(Line)) {}
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
/**
 * Settings: a cog.
 *
 * A cog and not three sliders-with-knobs, which is what this used to be and what
 * it looked like from across a room: three short horizontal strokes, each with a
 * dot on it, is the *overflow* mark — a list of things — so the button read as
 * "more options" and its being next to `refresh` made two buttons that both
 * looked like something else. The teeth are what makes it a cog: a ring alone is
 * a record, and a ring with eight radial strokes is unmistakably a gear.
 */
fun IconCog(tint: Color, box: Dp = 20.dp) {
    Canvas(Modifier.size(box)) {
        val w = size.minDimension
        val c = Offset(w / 2f, w / 2f)
        val stroke = w * 0.095f
        // Eight teeth. Each starts inside the ring's outer edge so the whole thing
        // reads as one object rather than a circle with spikes around it.
        for (i in 0 until 8) {
            val a = (i * 45.0) * PI / 180.0
            val ux = cos(a).toFloat()
            val uy = sin(a).toFloat()
            drawLine(
                tint,
                Offset(c.x + ux * w * 0.21f, c.y + uy * w * 0.21f),
                Offset(c.x + ux * w * 0.45f, c.y + uy * w * 0.45f),
                stroke,
                StrokeCap.Round,
            )
        }
        // The body, hollow: a filled disc would swallow the teeth's inner ends.
        drawCircle(tint, w * 0.30f, c, style = Stroke(stroke))
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
 * Two arrows chasing each other round a circle — a refresh mark.
 *
 * The two arcs are equal and sit opposite each other, and each one carries a head
 * at its end, so the ring reads as a ring with two turns in it rather than as a
 * circle with a line sticking out of each side. The heads are **V**s: two strokes
 * set back from the tip at the same angle as the tangent, which is what an arrow
 * actually looks like. A single barb, or a triangle, reads as a spike.
 */
private fun DrawScope.drawFlip(tint: Color) {
    val w = size.minDimension
    val stroke = w * 0.115f
    // Room for the heads to sit *outside* the arcs' radius without clipping.
    val inset = w * 0.24f
    val d = w - inset * 2f
    val gate = 52f      // degrees of gap at each head, so the heads have air
    val sweep = 180f - gate
    val style = Stroke(stroke, cap = StrokeCap.Round)
    for (start in listOf(-90f + gate / 2f, 90f + gate / 2f)) {
        val topLeft = Offset(inset, inset)
        drawArc(tint, start, sweep, false, topLeft, Size(d, d), style = style)
        arrowHead(tint, inset, d, start + sweep, w, stroke)
    }
}

/**
 * A V-shaped head at [angleDeg] on the circle, tangent to it, turning clockwise.
 *
 * Both barbs are swept back from the tip by the same angle, and both are shorter
 * than the gap between the two arcs — an arrow head wider than the gap it sits in
 * is what made the previous version look like a spike rather than an arrow.
 */
private fun DrawScope.arrowHead(
    tint: Color,
    inset: Float,
    d: Float,
    angleDeg: Float,
    w: Float,
    stroke: Float,
) {
    val r = d / 2f
    val cx = inset + r
    val cy = inset + r
    val a = angleDeg * (PI / 180.0)
    val co = cos(a).toFloat()
    val si = sin(a).toFloat()
    val tip = Offset(cx + r * co, cy + r * si)
    // Clockwise, with y growing downwards, the tangent is (-sin, cos).
    val tx = -si
    val ty = co
    // Long enough to see, short enough to stay inside the gap between the arcs.
    val len = w * 0.30f
    val barb = 42.0 * (PI / 180.0)
    val cs = cos(barb).toFloat()
    val sn = sin(barb)
    for (sign in listOf(1.0, -1.0)) {
        // Sweep the *backwards* tangent by ±barb: the two sides of the V.
        val s2 = (sn * sign).toFloat()
        val rx = -tx * cs - -ty * s2
        val ry = -tx * s2 + -ty * cs
        drawLine(tint, tip, Offset(tip.x + rx * len, tip.y + ry * len), stroke, StrokeCap.Round)
    }
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
/**
 * The title of the shell view: the folder the shell is in, not the way to it.
 *
 * `/home/ldeen/Documents/ldmux` is a *path*: it says where a thing is by spelling
 * the whole route from the root, which is worth the room when you are choosing a
 * directory and worth none of it when you are already in one. The shell view is
 * the already-in-one case, so it shows `ldmux`.
 *
 * Two `src` folders then look alike, which is the price and is payable: the
 * launcher is the screen where you tell shells apart, and it shows the full path
 * for exactly that reason.
 */
fun folderName(path: String): String {
  val home = Regex("^/(home|Users)/[^/]+").find(path)?.value
  if (home != null && path.trimEnd('/') == home) return "~"
  val base = path.trimEnd('/').substringAfterLast('/')
  return base.ifEmpty { if (home != null) "~" else "/" }
}

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
