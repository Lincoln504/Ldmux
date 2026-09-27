package dev.ldmux.mobile

/**
 * The pairing link, read (ADR-0017).
 *
 *     ldmux://pair?v=1&host=100.127.119.54:7625&token=<64 hex>&name=debian
 *
 * The mirror of `packages/protocol/src/pairing.ts`, and held to it by the fixture
 * both sides read (`packages/protocol/test/fixtures/pairing-links.json`, see
 * `PairingTest`). Anything this refuses, the CLI cannot produce, and the reverse.
 *
 * Refusing is the point of most of this. A pairing replaces where every keystroke
 * goes, and a QR code is untrusted input: a link to a host outside the tailnet is
 * not a pairing, whoever printed it.
 */
data class Pairing(val host: String, val token: String, val name: String)

const val PAIRING_VERSION = 1
private val TOKEN = Regex("^[0-9a-f]{64}$")
private const val NAME_MAX = 64

/** The pairing in [link], or null if it is not a valid ldmux pairing link. */
fun parsePairingLink(link: String): Pairing? {
    val s = link.trim()
    val prefix = "ldmux://pair"
    if (!s.startsWith(prefix, ignoreCase = true)) return null
    val rest = s.substring(prefix.length)
    // `ldmux://pair?…` only: `ldmux://pairing?…` or `ldmux://pair/x?…` are other links.
    if (!rest.startsWith("?")) return null
    val params = HashMap<String, String>()
    for (part in rest.substring(1).substringBefore('#').split('&')) {
        if (part.isEmpty()) continue
        val key = decodeQueryComponent(part.substringBefore('='))
        val value = decodeQueryComponent(part.substringAfter('=', ""))
        // First wins, like URLSearchParams.get on the other side.
        if (key !in params) params[key] = value
    }
    if (params["v"] != PAIRING_VERSION.toString()) return null
    val p = Pairing(host = params["host"] ?: "", token = params["token"] ?: "", name = params["name"] ?: "")
    return if (pairingProblem(p) == null) p else null
}

private fun pairingProblem(p: Pairing): String? {
    val hp = splitHost(p.host) ?: return "host"
    if (!isTailnetOrLoopback(hp.first)) return "host"
    if (!TOKEN.matches(p.token)) return "token"
    if (p.name.length > NAME_MAX || p.name.any { it.code < 0x20 || it.code == 0x7f }) return "name"
    return null
}

/** `address:port` (IPv6 bracketed) split in two, or null when it is not that shape. */
fun splitHost(host: String): Pair<String, Int>? {
    val m = Regex("""^(\[[0-9a-fA-F:.]+\]|[^:\[\]]+):(\d{1,5})$""").matchEntire(host) ?: return null
    val port = m.groupValues[2].toInt()
    if (port !in 1..65535) return null
    return m.groupValues[1] to port
}

/**
 * Form-style percent decoding, as `URLSearchParams` does it (WHATWG): `+` is a
 * space, `%XX` is a byte, a `%` not followed by two hex digits is itself, and the
 * bytes are read as UTF-8 with U+FFFD for anything invalid. Matching it exactly
 * is the point — this and the CLI's parser must agree on every input.
 */
internal fun decodeQueryComponent(s: String): String {
    val bytes = ArrayList<Byte>(s.length)
    var i = 0
    while (i < s.length) {
        val c = s[i]
        if (c == '+') {
            bytes.add(' '.code.toByte())
            i++
        } else if (c == '%' && i + 2 < s.length && s[i + 1].isHexDigitChar() && s[i + 2].isHexDigitChar()) {
            bytes.add(s.substring(i + 1, i + 3).toInt(16).toByte())
            i += 3
        } else {
            // A whole code point, so a character outside the BMP is one UTF-8
            // sequence rather than two lone surrogates.
            val len = if (c.isHighSurrogate() && i + 1 < s.length && s[i + 1].isLowSurrogate()) 2 else 1
            for (b in s.substring(i, i + len).encodeToByteArray()) bytes.add(b)
            i += len
        }
    }
    return bytes.toByteArray().decodeToString()
}

private fun Char.isHexDigitChar(): Boolean = this in '0'..'9' || this in 'a'..'f' || this in 'A'..'F'

// ── Tailnet only, as data (the mirror of packages/protocol/src/net.ts) ───────────

/**
 * Is [address] loopback or a tailnet address?
 *
 * Tailnet is `100.64.0.0/10` and `fd7a:115c:a1e0::/48`; loopback is `127.0.0.0/8`,
 * `::1` and `localhost`. Literal addresses only: a name would have to be resolved to
 * be checked.
 */
fun isTailnetOrLoopback(address: String): Boolean {
    var a = address.trim()
    if (a == "localhost") return true
    if (a.startsWith("[") && a.endsWith("]")) a = a.substring(1, a.length - 1)
    Regex("^::ffff:(\\d+\\.\\d+\\.\\d+\\.\\d+)$", RegexOption.IGNORE_CASE).matchEntire(a)?.let { a = it.groupValues[1] }
    parseIpv4(a)?.let { v4 ->
        if (v4[0] == 127) return true
        return v4[0] == 100 && v4[1] in 64..127
    }
    val v6 = parseIpv6(a) ?: return false
    if (v6.dropLast(1).all { it == 0 } && v6.last() == 1) return true
    return v6[0] == 0xfd7a && v6[1] == 0x115c && v6[2] == 0xa1e0
}

private fun parseIpv4(s: String): IntArray? {
    val parts = s.split('.')
    if (parts.size != 4) return null
    val out = IntArray(4)
    for ((i, p) in parts.withIndex()) {
        if (p.isEmpty() || p.length > 3 || !p.all { it.isDigit() }) return null
        if (p.length > 1 && p[0] == '0') return null
        val v = p.toInt()
        if (v > 255) return null
        out[i] = v
    }
    return out
}

/** Eight 16-bit groups, or null. No zone ids, no embedded IPv4 (neither is tailnet). */
private fun parseIpv6(s: String): IntArray? {
    if (s.isEmpty() || '%' in s || '.' in s) return null
    val halves = s.split("::")
    if (halves.size > 2) return null
    fun groups(h: String): List<Int>? {
        if (h.isEmpty()) return emptyList()
        return h.split(':').map { g ->
            if (g.isEmpty() || g.length > 4) return null
            g.toIntOrNull(16) ?: return null
        }
    }
    val head = groups(halves[0]) ?: return null
    if (halves.size == 1) return if (head.size == 8) head.toIntArray() else null
    val tail = groups(halves[1]) ?: return null
    val fill = 8 - head.size - tail.size
    if (fill < 1) return null
    return (head + List(fill) { 0 } + tail).toIntArray()
}
