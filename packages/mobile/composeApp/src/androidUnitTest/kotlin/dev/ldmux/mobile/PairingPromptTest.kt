package dev.ldmux.mobile

import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFalse
import kotlin.test.assertTrue

/**
 * The pairing confirmation: a QR code is untrusted input and its name is shown in the
 * one dialog that decides where every keystroke goes.
 */
class PairingPromptTest {
    private val token = "0123456789abcdef".repeat(4)
    private fun pairing(name: String = "debian", host: String = "100.127.119.54:7625") = Pairing(host, token, name)

    @Test
    fun `the address is always shown, with or without a name`() {
        assertEquals("100.127.119.54:7625", pairingPrompt(pairing(name = "work laptop")).address)
        assertEquals("100.127.119.54:7625", pairingPrompt(pairing(name = "")).address)
    }

    @Test
    fun `bidi overrides and isolates are stripped`() {
        // U+202E reverses what follows; U+2066-2069 isolate it. Either can make the name
        // read as something else next to the address.
        val evil = "‮gnp.exe‬ trusted⁦x⁩"
        val clean = sanitizeDisplayName(evil)
        for (c in "‪‫‬‭‮⁦⁧⁨⁩‎‏") {
            assertFalse(c in clean, "U+${c.code.toString(16)} survived")
        }
        assertEquals("gnp.exe trustedx", clean)
    }

    @Test
    fun `zero-width and format characters are stripped`() {
        assertEquals("ab", sanitizeDisplayName("a​b‌‍⁠﻿"))
    }

    @Test
    fun `control characters and separators are stripped`() {
        assertEquals("a b", sanitizeDisplayName("a\u0007\u0000 \nb\u007f\u0085  "))
    }

    @Test
    fun `whitespace collapses and the ends are trimmed`() {
        assertEquals("a b c", sanitizeDisplayName("  a   b\t\tc  "))
    }

    @Test
    fun `ordinary international text and emoji survive`() {
        assertEquals("café 日本語 😀", sanitizeDisplayName("café 日本語 😀"))
    }

    @Test
    fun `private use and unassigned code points are stripped`() {
        assertEquals("ab", sanitizeDisplayName("ab￿"))
    }

    @Test
    fun `a name of only invisible characters becomes empty, so the dialog shows the address alone`() {
        assertEquals("", sanitizeDisplayName("‮​\u0007"))
        assertEquals("", pairingPrompt(pairing(name = "‮​")).name)
    }

    @Test
    fun `the name is capped so it cannot push the address out of view`() {
        assertEquals(64, sanitizeDisplayName("x".repeat(500)).length)
        // ...without splitting a surrogate pair at the cut.
        val out = sanitizeDisplayName("x".repeat(63) + "😀")
        assertTrue(out.isEmpty() || !out.last().isHighSurrogate())
    }

    @Test
    fun `the fingerprint is the first and last four digits, not the secret`() {
        assertEquals("0123…cdef", tokenFingerprint(token))
        assertEquals("", tokenFingerprint("abc"))
        assertTrue(token !in tokenFingerprint(token))
    }

    // ---- where a bearer token may be sent --------------------------------------------

    @Test
    fun `tailnet and loopback literals are endpoints, nothing else is`() {
        for (h in listOf("100.64.0.1:7625", "100.127.119.54:7625", "localhost:7629", "127.0.0.1:7629", "[::1]:7625", "[fd7a:115c:a1e0::1]:7625")) {
            assertTrue(isAllowedEndpointHost(h), h)
        }
        for (h in listOf("8.8.8.8:7625", "100.128.0.1:7625", "192.168.1.5:7625", "debian:7625", "example.com:80", "100.64.0.1", "", "100.64.0.1:0")) {
            assertFalse(isAllowedEndpointHost(h), h)
        }
    }

    @Test
    fun `a host that is not a tailnet address reads as unpaired`() {
        // A host saved by an older version, or anything that bypassed pairing, must not
        // be sent a bearer token in cleartext just because a token is stored with it.
        assertTrue(LdmuxConfig(host = "100.64.0.9:7625", token = token).isPaired)
        assertFalse(LdmuxConfig(host = "203.0.113.7:7625", token = token).isPaired)
        assertFalse(LdmuxConfig(host = "debian:7625", token = token).isPaired)
        assertEquals(null, LdmuxConfig(host = "203.0.113.7:7625", token = token).endpoint)
    }
}
