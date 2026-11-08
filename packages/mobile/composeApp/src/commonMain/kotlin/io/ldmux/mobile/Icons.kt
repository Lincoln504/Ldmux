package io.ldmux.mobile

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
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
import androidx.compose.ui.graphics.PathFillType
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.StrokeJoin
import androidx.compose.ui.graphics.drawscope.DrawScope
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.material3.Text
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
    /**
     * What this button is, spoken.
     *
     * Required rather than defaulted, because these buttons are *drawn*: the glyph
     * is a Canvas arc or a path and carries no text at all, so a control with no
     * label here has no name anywhere. That is measured, not assumed — a
     * `uiautomator` dump of the running app lists the text keys and the arrows
     * (`\u2190`, `\u2191`, `\u2193`, `\u2192`, `\u23ce`, `\u25b6`) and reports every one of these
     * buttons as an unlabelled square. Text keys read themselves; these cannot.
     *
     * A default would let the next button be added without one, which is exactly how
     * this gap arises.
     */
    label: String,
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
            .clip(RadiusMedium)
            .background(if (filled) Raised else Bg)
            // Android's minimum touch target, and nothing in this app specified one.
            // These buttons came to ≈30–34 dp, which is small enough to miss —
            // including the close button, which is destructive and sits nested inside
            // a row that is itself tappable.
            .heightIn(min = LocalTouch.current.min)
            .clickable(onClick = onClick, role = Role.Button)
            .semantics { contentDescription = label }
            .padding(horizontal = 9.dp, vertical = LocalTouch.current.pad),
        // A Column arranges from the top by default. While this box was exactly its
        // content's height that was unobservable, and it stopped being unobservable
        // the moment heightIn(min = MinTouch) made the box taller than the icon:
        // every drawn control in the app quietly moved to the top of its own button.
        // Measured on the landscape panel, the menu glyph's grid sat 22 px above the
        // centre of the button it lives in. It is one line, it was every icon here.
        verticalArrangement = Arrangement.Center,
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        content()
        if (caption != null) {
            // 9sp and the quiet grey: this sits under a glyph inside a control the size of a
            // key, so it has to stay small — but 8sp was under the size at which the shapes
            // survive at all, and it is still read, not decoration.
            Text(caption, color = Muted, fontFamily = MonoFont, fontSize = 9.sp)
        }
    }
}

/** A one-pixel rule that separates control groups. */
@Composable
fun PanelDivider() {
    Column(Modifier.fillMaxWidth().height(1.dp).background(Line)) {}
}

/** A keyboard and nothing else: the portrait bar's show/hide toggle. */
@Composable
fun IconKeyboard(tint: Color, box: Dp = 22.dp) {
    Canvas(Modifier.size(box)) { drawKeyboard(tint) }
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

/**
 * A phone, for marking a shell that was opened from one.
 *
 * Drawn rather than typed, like every other icon here: a font's phone symbol is a
 * different picture on every device, and at 12dp beside 14sp text it has to be the
 * same picture everywhere. It takes a modifier so the caller can describe it — a
 * picture is not a description, and this one carries information.
 */
@Composable
fun IconPhone(tint: Color, box: Dp = 12.dp, modifier: Modifier = Modifier) {
    Canvas(modifier.size(box)) {
        val w = size.minDimension
        val stroke = w * 0.11f
        drawRoundRect(
            color = tint,
            topLeft = Offset(w * 0.22f, w * 0.06f),
            size = Size(w * 0.56f, w * 0.88f),
            cornerRadius = CornerRadius(w * 0.16f, w * 0.16f),
            style = Stroke(width = stroke),
        )
        // The speaker. A rounded slab is a slab; this is what makes it a phone.
        drawLine(tint, Offset(w * 0.38f, w * 0.19f), Offset(w * 0.62f, w * 0.19f), stroke, StrokeCap.Round)
    }
}

/**
 * "Open this somewhere else": a window with an arrow leaving its top-right corner.
 *
 * The idiom every desktop uses for "open in a new window", and it stands in for five
 * words of text on every launcher row.
 *
 * Redrawn: the first version rounded one corner and put the arrow over the box's own
 * lines, which at 18dp is a smudge. Every corner it has is now rounded by the same
 * radius, the top-right corner is genuinely absent so the arrow leaves through a gap,
 * and the arrow's shaft starts clear of the box rather than crossing it.
 */
@Composable
fun IconPopOut(tint: Color, box: Dp = 18.dp) {
    Canvas(Modifier.size(box)) {
        val w = size.minDimension
        val stroke = w * 0.13f
        val r = w * 0.15f
        // The window: three corners with the same radius, and the fourth simply not
        // drawn, so the arrow has somewhere to go.
        drawPath(
            Path().apply {
                moveTo(w * 0.82f, w * 0.36f)
                lineTo(w * 0.82f, w * 0.82f - r)
                quadraticTo(w * 0.82f, w * 0.82f, w * 0.82f - r, w * 0.82f)
                lineTo(w * 0.18f + r, w * 0.82f)
                quadraticTo(w * 0.18f, w * 0.82f, w * 0.18f, w * 0.82f - r)
                lineTo(w * 0.18f, w * 0.18f + r)
                quadraticTo(w * 0.18f, w * 0.18f, w * 0.18f + r, w * 0.18f)
                lineTo(w * 0.56f, w * 0.18f)
            },
            tint,
            style = Stroke(width = stroke, cap = StrokeCap.Round, join = StrokeJoin.Round),
        )
        // The arrow, entirely outside the box's ink.
        drawLine(tint, Offset(w * 0.54f, w * 0.46f), Offset(w * 0.86f, w * 0.14f), stroke, StrokeCap.Round)
        drawLine(tint, Offset(w * 0.86f, w * 0.14f), Offset(w * 0.60f, w * 0.14f), stroke, StrokeCap.Round)
        drawLine(tint, Offset(w * 0.86f, w * 0.14f), Offset(w * 0.86f, w * 0.40f), stroke, StrokeCap.Round)
    }
}

/**
 * Settings: the ordinary gear.
 *
 * A filled wheel with eight square teeth and a round hole, the shape every
 * platform uses for settings. The earlier outline ring with eight radial strokes
 * read as a sun or a spark at 20dp, not as a gear.
 */
@Composable
fun IconCog(tint: Color, box: Dp = 20.dp) {
    Canvas(Modifier.size(box)) {
        val w = size.minDimension
        val c = Offset(w / 2f, w / 2f)
        val teeth = 8
        val outer = w * 0.47f
        val body = w * 0.36f
        // Each tooth spans this fraction of its sector at the tip, a bit more at
        // the root, so it tapers like a real tooth.
        val tip = 0.36f
        val root = 0.52f
        val sector = 2.0 * PI / teeth
        fun at(r: Float, a: Double) = Offset(c.x + r * cos(a).toFloat(), c.y + r * sin(a).toFloat())
        val gear = Path().apply {
            for (i in 0 until teeth) {
                val mid = i * sector - PI / 2
                val r0 = at(body, mid - sector * root / 2)
                if (i == 0) moveTo(r0.x, r0.y) else lineTo(r0.x, r0.y)
                at(outer, mid - sector * tip / 2).let { lineTo(it.x, it.y) }
                at(outer, mid + sector * tip / 2).let { lineTo(it.x, it.y) }
                at(body, mid + sector * root / 2).let { lineTo(it.x, it.y) }
                // The body between teeth, as a short arc of the wheel's rim.
                val next = (i + 1) * sector - PI / 2 - sector * root / 2
                val steps = 3
                val from = mid + sector * root / 2
                for (k in 1..steps) at(body, from + (next - from) * k / steps).let { lineTo(it.x, it.y) }
            }
            close()
            addOval(Rect(c, w * 0.15f))
            fillType = PathFillType.EvenOdd
        }
        drawPath(gear, tint)
    }
}

/**
 * Refresh: one arrow going most of the way round a circle.
 *
 * The universal mark for "fetch this again". A gap at the top, which the
 * arrowhead points into, so the circle reads as a motion rather than a ring.
 */
@Composable
fun IconRefresh(tint: Color, box: Dp = 20.dp) {
    Canvas(Modifier.size(box)) {
        val w = size.minDimension
        val stroke = w * 0.11f
        val r = w * 0.31f
        val c = Offset(w / 2f, w / 2f)
        // Clockwise from the top-right round to the top-left, leaving the gap at
        // the top that the head points into.
        val start = -50f
        val sweep = 280f
        drawArc(
            tint,
            start,
            sweep,
            false,
            Offset(c.x - r, c.y - r),
            Size(r * 2f, r * 2f),
            style = Stroke(stroke, cap = StrokeCap.Round),
        )
        // The head, at the end of the arc, pointing on along it (clockwise).
        val a = (start + sweep) * PI / 180.0
        val end = Offset(c.x + r * cos(a).toFloat(), c.y + r * sin(a).toFloat())
        // Clockwise tangent in screen coordinates.
        val tx = -sin(a).toFloat()
        val ty = cos(a).toFloat()
        // Radial direction, for the head's width.
        val nx = cos(a).toFloat()
        val ny = sin(a).toFloat()
        val len = w * 0.30f
        val half = w * 0.19f
        val headTip = Offset(end.x + tx * len * 0.6f, end.y + ty * len * 0.6f)
        val back = Offset(end.x - tx * len * 0.4f, end.y - ty * len * 0.4f)
        drawPath(
            Path().apply {
                moveTo(headTip.x, headTip.y)
                lineTo(back.x + nx * half, back.y + ny * half)
                lineTo(back.x - nx * half, back.y - ny * half)
                close()
            },
            tint,
        )
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
 * The way out of this orientation: a double-headed arrow on the diagonal.
 *
 * It was two arcs chasing each other round a circle, which is the **refresh** mark
 * — and a refresh mark promises that something will be fetched again, which is not
 * what this does. A plain left-right arrow tilted forty-five degrees says the one
 * thing that is true: the screen is going to turn, and it comes back the same way.
 *
 * Drawn along the diagonal rather than drawn flat and rotated: the geometry is two
 * points either way, and a rotated canvas only adds a chance to get the two heads
 * mismatched.
 *
 * Each head carries **one** barb, not two. Two barbs per head is a full arrowhead,
 * and a pair of full arrowheads facing away from each other is a *flip*: a bar with
 * a head on each end, which is what a mirror does. Taking one barb off each turns
 * the pair into two hooks leaning the same way round, which is what rotation looks
 * like. There are only two ways to choose the remaining pair and they are mirror
 * images of each other, so the choice is which way it reads as turning: the two kept
 * here sit 45 degrees off the clockwise tangent at *both* ends, so the mark reads as
 * a clockwise sweep. The other pair is the same drawing turning the other way.
 *
 * They are also the axis-aligned pair, which matters at 18-22dp: an axis-aligned
 * stroke lands on whole pixels where a diagonal one is antialiased on both edges.
 */


/**
 * A tick, drawn rather than typed.
 *
 * It was `Text("\u2713")` in an 18dp box, which is two problems in one line. A
 * tick is a mark, and every other mark in this app is drawn — a font's tick is a
 * different picture on every device, and this one also had to be trusted to fit
 * inside a box with a 1.5dp border drawn by something else. It did not: the ink is
 * the glyph's, at whatever size and baseline the font decides, so the box and the
 * mark were sized by two different authorities and only one of them knew about the
 * border.
 *
 * Drawn, the fit is arithmetic. The ink spans 0.20 to 0.80 of the box on one axis
 * and 0.28 to 0.74 on the other, plus half a stroke all round, which inside a 12dp
 * box leaves better than a dp of clear space inside the border at every edge.
 */
@Composable
fun IconCheck(tint: Color, box: Dp = 12.dp) {
    Canvas(Modifier.size(box)) {
        val w = size.minDimension
        val stroke = w * 0.16f
        val elbow = Offset(w * 0.42f, w * 0.72f)
        drawLine(tint, Offset(w * 0.21f, w * 0.52f), elbow, stroke, StrokeCap.Round)
        drawLine(tint, elbow, Offset(w * 0.79f, w * 0.29f), stroke, StrokeCap.Round)
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
 * The title of the shell view: the folder the shell is in, not the way to it.
 *
 * `/home/user/src/ldmux` is a *path*: it says where a thing is by spelling
 * the whole route from the root, which is worth the room when you are choosing a
 * directory and worth none of it when you are already in one. The shell view is
 * the already-in-one case, so it shows `ldmux`.
 *
 * Two `src` folders then look alike, which is the price and is payable: the
 * launcher is the screen where you tell shells apart, and it shows the full path
 * for exactly that reason.
 */
fun folderName(path: String): String {
  // An empty path is "not known", not "the filesystem root": the shell header puts this beside a
  // session's name, and a lone "/" there says the shell is at the root of the filesystem, which is
  // a false statement about where it is. A session that has not reported a cwd (or reports one the
  // host could not read) is a shell whose folder is unknown, so it says the one thing that is true.
  val trimmed = path.trim()
  if (trimmed.isEmpty()) return "shell"
  val home = Regex("^/(home|Users)/[^/]+").find(trimmed)?.value
  if (home != null && trimmed.trimEnd('/') == home) return "~"
  val base = trimmed.trimEnd('/').substringAfterLast('/')
  return base.ifEmpty { if (home != null) "~" else "/" }
}

fun tildify(path: String): String {
    val m = Regex("^/(home|Users)/[^/]+").find(path) ?: return path
    return "~" + path.removePrefix(m.value)
}
