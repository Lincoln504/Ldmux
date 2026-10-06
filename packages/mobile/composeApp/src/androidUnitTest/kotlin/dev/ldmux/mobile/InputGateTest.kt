package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * Input while offline used to be painted by prediction and then silently lost. The gate
 * is the whole rule: no live socket, no input, and above all no pretending.
 */
class InputGateTest {
    private fun gate(
        connected: Boolean = true,
        held: Boolean = false,
        predict: Boolean = true,
        alt: Boolean = false,
        password: Boolean = false,
    ) = inputGate(connected, held, predict, alt, password)

    @Test
    fun `connected input is sent and, if enabled, predicted`() {
        assertEquals(InputGate(canSend = true, canPredict = true), gate())
    }

    @Test
    fun `offline input is refused and never predicted`() {
        val g = gate(connected = false)
        assertFalse(g.canSend)
        assertFalse(g.canPredict)
    }

    @Test
    fun `a shell held elsewhere refuses input too`() {
        val g = gate(held = true)
        assertFalse(g.canSend)
        assertFalse(g.canPredict)
    }

    @Test
    fun `the existing prediction gates still apply while connected`() {
        assertTrue(gate(predict = false).canSend)
        assertFalse(gate(predict = false).canPredict)
        assertFalse(gate(alt = true).canPredict)
        assertFalse(gate(password = true).canPredict)
        assertTrue(gate(alt = true).canSend)
    }

    @Test
    fun `prediction is never allowed when sending is not`() {
        for (c in listOf(true, false)) for (h in listOf(true, false)) for (p in listOf(true, false)) {
            val g = gate(connected = c, held = h, predict = p)
            if (!g.canSend) assertFalse(g.canPredict)
        }
    }
}
