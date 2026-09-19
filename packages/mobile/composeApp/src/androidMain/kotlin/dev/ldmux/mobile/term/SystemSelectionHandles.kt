package dev.ldmux.mobile.term

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.drawable.Drawable
import android.util.TypedValue
import androidx.compose.ui.graphics.ImageBitmap
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.unit.IntSize

/**
 * The platform's own text-selection handles, as bitmaps, in a greyscale tint.
 *
 * The app draws every pixel of its terminal, so it has no `TextView` and therefore none
 * of the selection furniture that comes with one: the framework's handles live inside
 * `android.widget.Editor`, which is `@hide`. The *art*, however, is public — it is the
 * theme attributes `textSelectHandle`, `textSelectHandleLeft` and `textSelectHandleRight`
 * — so resolving them yields exactly what the system's own text fields show, at the
 * platform's own shape.
 *
 * Three things here are deliberate, and each was wrong first:
 *
 * 1. **The aspect ratio is the asset's, not ours.** These were drawn at a fixed 48x32 dp,
 *    which squashed a roughly 2:1 teardrop into 3:2 and made it read as a fat blob. The
 *    height is chosen and the width is derived from the drawable, so the shape is the
 *    framework's whatever the theme supplies.
 *
 * 2. **A modern theme has no separate left and right asset.** It ships one teardrop and
 *    `Editor` mirrors it for the start. Drawing it twice unmirrored gives two handles
 *    leaning the same way, which is what the phone showed. So: use the left/right pair
 *    when the theme has them, and otherwise mirror one for the start handle.
 *
 * 3. **The tint is ours.** The framework assets come tinted with
 *    `?attr/colorControlActivated`, which is the system accent and is blue. This app has
 *    no accent hue at all, so the shape is the platform's and the colour is the app's.
 */
class SystemSelectionHandles private constructor(
    val start: ImageBitmap,
    val end: ImageBitmap,
    val sizePx: IntSize,
    /** True when [start] must be flipped horizontally, because it is a shared asset. */
    val mirrorStart: Boolean,
) {
    companion object {
        /**
         * The framework's own design height for these handles.
         *
         * 48x24 dp is what the AOSP assets are built at, and the framework draws them at
         * their intrinsic size. The width follows the asset.
         */
        private const val HEIGHT_DP = 24f

        /**
         * Answers null when the theme has no such attribute. That is not an error: the
         * handles are then simply not drawn and are still draggable, because the geometry
         * the hit test uses does not depend on the art.
         */
        fun load(context: Context, tintArgb: Int, heightPx: Int): SystemSelectionHandles? {
            if (heightPx <= 0) return null
            val left = resolve(context, android.R.attr.textSelectHandleLeft)
            val right = resolve(context, android.R.attr.textSelectHandleRight)
            val single = resolve(context, android.R.attr.textSelectHandle)

            val startDrawable: Drawable
            val endDrawable: Drawable
            var mirrorStart = false
            if (left != null && right != null) {
                startDrawable = left
                endDrawable = right
            } else if (single != null) {
                startDrawable = single
                endDrawable = single
                mirrorStart = true
            } else {
                return null
            }

            val start = rasterise(startDrawable, tintArgb, heightPx) ?: return null
            val end = rasterise(endDrawable, tintArgb, heightPx) ?: return null
            return SystemSelectionHandles(
                start = start.asImageBitmap(),
                end = end.asImageBitmap(),
                sizePx = IntSize(start.width, start.height),
                mirrorStart = mirrorStart,
            )
        }

        private fun resolve(context: Context, attr: Int): Drawable? {
            val value = TypedValue()
            if (!context.theme.resolveAttribute(attr, value, true)) return null
            if (value.resourceId == 0) return null
            return context.getDrawable(value.resourceId)
        }

        /**
         * Draw the drawable into a bitmap at its own aspect ratio, tinted.
         *
         * Hand-rolled rather than `Drawable.toBitmap()` so this needs no androidx-core
         * dependency, and drawn through `setBounds` so a nine-patch stretches properly
         * instead of being resampled as a flat bitmap.
         */
        private fun rasterise(drawable: Drawable, tintArgb: Int, heightPx: Int): Bitmap? {
            val intrinsicW = drawable.intrinsicWidth
            val intrinsicH = drawable.intrinsicHeight
            if (intrinsicW <= 0 || intrinsicH <= 0) return null
            val w = (heightPx * (intrinsicW.toFloat() / intrinsicH.toFloat())).toInt()
            if (w <= 0) return null
            val bitmap = Bitmap.createBitmap(w, heightPx, Bitmap.Config.ARGB_8888)
            drawable.setTint(tintArgb)
            drawable.setBounds(0, 0, w, heightPx)
            drawable.draw(Canvas(bitmap))
            return bitmap
        }
    }
}
