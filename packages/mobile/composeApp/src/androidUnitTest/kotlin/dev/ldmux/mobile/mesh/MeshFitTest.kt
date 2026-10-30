package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.LdmuxConfig
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertTrue

class MeshFitTest {
    @Test
    fun `a grid that fits at the chosen size keeps that size`() {
        assertEquals(11f, meshFitFontSp(widthDp = 800f, cols = 80, fontScale = 1f, wanted = 11f))
    }

    @Test
    fun `an 80 column host on a phone shrinks the text until the whole line fits`() {
        val sp = meshFitFontSp(widthDp = 432f, cols = 80, fontScale = 1f, wanted = 14f)
        assertTrue(sp < 14f)
        // The grid's width at that size must not exceed the width we have.
        assertTrue(sp * 0.62f * 80 <= 432f + 0.01f)
    }

    @Test
    fun `the system font scale is accounted for, since the size is in sp`() {
        val plain = meshFitFontSp(432f, 80, 1f, 20f)
        val scaled = meshFitFontSp(432f, 80, 1.3f, 20f)
        assertTrue(scaled < plain)
    }

    @Test
    fun `never below the app's minimum, clipping is the honest result past that`() {
        assertEquals(LdmuxConfig.MIN_FONT_SP, meshFitFontSp(100f, 200, 1f, 14f))
    }

    @Test
    fun `no grid or no width is no opinion`() {
        assertEquals(12f, meshFitFontSp(432f, 0, 1f, 12f))
        assertEquals(12f, meshFitFontSp(0f, 80, 1f, 12f))
    }
}
