package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertContentEquals
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * Predictive echo is a state machine that decides what the user sees before the
 * server answers, so its edges are the product: a wrong verdict here is a
 * flickering or double-printed terminal, or a password drawn on screen.
 */
class PredictorTest {

    private fun bytes(s: String) = s.encodeToByteArray()

    @Test
    fun `nothing outstanding is a pass-through`() {
        val p = Predictor()
        val out = bytes("hello")
        val c = p.reconcile(out)
        assertTrue(c is Predictor.Consumption.Passthrough)
    }

    @Test
    fun `a confirmed echo is consumed and only the remainder is written`() {
        val p = Predictor()
        p.predict(bytes("ls"))
        // The shell echoes "ls" then prints the command's output.
        val c = p.reconcile(bytes("ls\r\nfile1\r\n"))
        assertTrue(c is Predictor.Consumption.Consume)
        assertContentEquals(bytes("\r\nfile1\r\n"), c.remaining)
        assertFalse(p.hasPending)
        assertTrue(p.echoing)
    }

    @Test
    fun `an echo split across frames is consumed without writing anything`() {
        val p = Predictor()
        p.predict(bytes("echo hi"))
        val first = p.reconcile(bytes("ech"))
        assertTrue(first is Predictor.Consumption.Consume)
        assertContentEquals(ByteArray(0), first.remaining)
        assertTrue(p.hasPending, "the rest of the prediction is still outstanding")

        val second = p.reconcile(bytes("o hi"))
        assertTrue(second is Predictor.Consumption.Consume)
        assertContentEquals(ByteArray(0), second.remaining)
        assertFalse(p.hasPending)
    }

    @Test
    fun `divergence is a mispredict and stops predicting`() {
        val p = Predictor()
        p.predict(bytes("abc"))
        val c = p.reconcile(bytes("aXY"))
        assertTrue(c is Predictor.Consumption.Mispredict)
        assertFalse(p.hasPending)
        // Having been wrong, we must not predict again until the line is
        // submitted — this is the password guard.
        assertFalse(p.canPredict(bytes("z")))
        p.onLineSubmitted()
        assertTrue(p.canPredict(bytes("z")))
    }

    @Test
    fun `abandoning a stale prediction stops predicting`() {
        val p = Predictor()
        p.predict(bytes("secret"))
        p.abandon()
        assertFalse(p.hasPending)
        assertFalse(p.echoing)
        assertFalse(p.canPredict(bytes("x")))
    }

    @Test
    fun `submit re-arms prediction and drops the old line`() {
        val p = Predictor()
        p.predict(bytes("partial"))
        p.onLineSubmitted()
        assertFalse(p.hasPending)
        assertTrue(p.echoing)
    }

    @Test
    fun `control bytes and escapes are never predicted`() {
        val p = Predictor()
        assertFalse(p.canPredict(bytes("\u0003")), "Ctrl-C")
        assertFalse(p.canPredict(bytes("\n")), "newline")
        assertFalse(p.canPredict(bytes("\u001b[A")), "arrow key")
        assertFalse(p.canPredict(ByteArray(0)))
        // TAB is a *named key* in our protocol, never literal text, so it is not
        // a prediction candidate either way.
        assertFalse(p.canPredict(bytes("a b\tc")))
        assertTrue(p.canPredict(bytes("a b c")))
        assertTrue(p.canPredict(bytes("héllo")))
    }

    @Test
    fun `a mispredict after a partial match still consumes the matched bytes`() {
        // The shell echoed "ab" of "abc" and then something unrelated happened.
        val p = Predictor()
        p.predict(bytes("abc"))
        val c = p.reconcile(bytes("abZ"))
        assertTrue(c is Predictor.Consumption.Mispredict)
        assertFalse(p.hasPending, "the whole prediction is dropped, not left half-consumed")
    }

    @Test
    fun `a mispredict hands back every byte it was given`() {
        // This is the one branch where output is neither written through nor
        // partially consumed, so it is the one branch that can lose a frame. It
        // did: the controller repainted from a snapshot and dropped the bytes, and
        // a snapshot is a rendered screen that cannot carry an escape sequence —
        // so a program's clipboard write vanished while the program was told it
        // had succeeded. The bytes travel in the verdict now, and this pins the
        // exact bytes: a frame ending mid-escape is still delivered whole.
        val p = Predictor()
        p.predict(bytes("abc"))
        val frame = bytes("abZ\u001b]52;c;aGVsbG8=\u0007tail")
        val c = p.reconcile(frame)
        assertTrue(c is Predictor.Consumption.Mispredict)
        val verdict = c as Predictor.Consumption.Mispredict
        assertContentEquals(frame, verdict.feed, "a mispredict must not swallow the frame")
    }

    @Test
    fun `a mispredict with no prediction at all never happens`() {
        // Guards the empty case: with nothing outstanding, reconcile is a
        // pass-through, so an escape sequence is written straight through.
        val p = Predictor()
        val frame = bytes("\u001b]52;c;aGVsbG8=\u0007")
        val c = p.reconcile(frame)
        assertTrue(c is Predictor.Consumption.Passthrough)
    }
}