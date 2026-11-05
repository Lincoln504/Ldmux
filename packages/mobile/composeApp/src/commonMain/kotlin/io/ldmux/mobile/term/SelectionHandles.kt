package io.ldmux.mobile.term

import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.unit.IntSize
import io.ldmux.mobile.term.TerminalFrame.SelectionBounds

/**
 * A finger's hold on one end of a selection.
 *
 * [start] is which end, and [offset] is where inside the handle the finger landed,
 * expressed as the vector from the finger to the handle's anchor. The drag then
 * reports the finger *plus* this offset, so the end does not jump under the thumb the
 * instant the drag begins — which is the difference between a handle that feels
 * attached to the text and one that feels like it is being thrown.
 */
data class HandleGrab(val start: Boolean, val offset: Offset)

/**
 * Where each handle's anchor sits, in surface pixels: start first, then end.
 *
 * The anchor is the *corner* of the cell the handle belongs to — the bottom-left of the
 * first selected cell and the bottom-right of the last, which is one column past it —
 * mirroring how the platform's own handles sit at the ends of the line they describe.
 * One function answers for both the drawing and the hit test, so a handle is always
 * exactly where it looks.
 */
fun selectionHandleAnchors(
    bounds: SelectionBounds,
    placement: GridPlacement,
): Pair<Offset, Offset> {
    val start = Offset(
        placement.dx + bounds.startCol * placement.cellWidth,
        placement.dy + (bounds.startRow + 1) * placement.cellHeight,
    )
    val end = Offset(
        placement.dx + (bounds.endCol + 1) * placement.cellWidth,
        placement.dy + (bounds.endRow + 1) * placement.cellHeight,
    )
    return start to end
}

/**
 * Where the platform handle's bounding box goes, given its anchor.
 *
 * This is the framework's own convention and it is not ours to improve on: the left
 * handle's hotspot sits at three quarters of its width and the right handle's at one
 * quarter, so each teardrop points *inwards* at the text it belongs to and the grab
 * point is its tip. The tip is the anchor, the handle hangs below it, and that is the
 * whole layout.
 */
fun selectionHandleTopLeft(
    anchor: Offset,
    handleSize: IntSize,
    start: Boolean,
): Offset = Offset(
    x = anchor.x - handleSize.width * (if (start) 0.75f else 0.25f),
    y = anchor.y,
)

/**
 * Which end of the selection a finger at [pos] has hold of, or null if neither.
 *
 * The test is against the ball, and against the anchor, and both are generous: a handle
 * is a small thing on a phone and the app's rule is that a control is at least as big as
 * a fingertip ([touchPx]). Measured from the same geometry the drawing uses, because a
 * hit test that is not the exact inverse of the draw is the recorded failure mode — a
 * handle you can see and cannot grab.
 */
fun handleHit(
    pos: Offset,
    bounds: SelectionBounds,
    placement: GridPlacement,
    handleSize: IntSize,
    touchPx: Float,
): HandleGrab? {
    if (placement.cellWidth <= 0f || placement.cellHeight <= 0f) return null
    val (startAnchor, endAnchor) = selectionHandleAnchors(bounds, placement)

    fun grab(anchor: Offset, start: Boolean): HandleGrab? {
        val topLeft = selectionHandleTopLeft(anchor, handleSize, start)
        // The handle's own box, inflated a little. The framework's touch target is the
        // visual with no slop at all — a 48dp-wide target, which is already the app's
        // own minimum — so the inflation only forgives a finger that lands a few pixels
        // outside the drawn shape rather than enlarging the shape itself.
        val slop = touchPx * 0.2f
        val within = pos.x >= topLeft.x - slop &&
            pos.x <= topLeft.x + handleSize.width + slop &&
            pos.y >= topLeft.y - slop &&
            pos.y <= topLeft.y + handleSize.height + slop
        // The tip counts on its own: a finger on the corner of the selection has
        // unambiguously chosen that end even if it misses the box by a hair.
        val onTip = (pos - anchor).getDistance() <= touchPx * 0.35f
        return if (within || onTip) HandleGrab(start, anchor - pos) else null
    }

    // Start first only because it is the earlier end; both are tested, and they
    // cannot overlap because the balls splay outward from the opposite corners of
    // the range and the range is never narrower than a cell.
    return grab(startAnchor, true) ?: grab(endAnchor, false)
}
