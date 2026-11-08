package io.ldmux.mobile
import io.ldmux.mobile.mesh.RadioPreset

/**
 * The pairing link, read (ADR-0017).
 *
 *     ldmux://pair?v=1&host=100.100.0.1:7625&token=<64 hex>&name=<hostname>
 *
 * The mirror of `crates/protocol/src/pairing.rs`, and held to it by the fixture
 * both sides read (`crates/protocol/test/fixtures/pairing-links.json`, see
 * `PairingTest`). Anything this refuses, the CLI cannot produce, and the reverse.
 *
 * Refusing is the point of most of this. A pairing replaces where every keystroke
 * goes, and a QR code is untrusted input: a link to a host outside the tailnet is
 * not a pairing, whoever printed it.
 */
data class Pairing(
    val host: String,
    val token: String,
    val name: String,
    /**
     * The mesh route's keys, when the link carried them (`mv=1&mk=<64 hex>&psk=<64 hex>`). Null for
     * every link a pre-mesh CLI makes, and then the pairing is exactly what it always was.
     */
    val mesh: MeshKeyMaterial? = null,
) {
    /**
     * A link made with `ldmux code --mesh-only`: the mesh keys and a name, **no daemon address
     * and no daemon bearer token**. It cannot pair the tailnet (there is nothing to connect to),
     * and a phone given one holds no credential that grants shell control over the tailnet.
     */
    val isMeshOnly: Boolean get() = host.isEmpty() && token.isEmpty() && mesh != null
}

/**
 * What a pairing link hands the phone for the mesh route: the daemon's session public key and
 * the pairing's pre-shared secret (ADR-0020). Both are credentials: never in [toString], never
 * logged. Equality is by content so a pairing compares as a value.
 */
class MeshKeyMaterial(
    val hostKey: ByteArray,
    val psk: ByteArray,
    /** The host's MeshCore radio public key, which the gateway binds. Null when the daemon was not told it. */
    val hostRadio: ByteArray? = null,
    /**
     * The tuple the host's radio is tuned to, when the code carries it.
     *
     * Two radios on different tuples cannot hear each other, and the phone had no way to learn the
     * host's: a code without it left the user comparing two screens by eye, with a mismatch looking
     * exactly like being out of range. The app shows it beside the phone's own and offers one press
     * to match.
     */
    val hostPreset: RadioPreset? = null,
) {
    init {
        require(hostKey.size == 32 && psk.size == 32 && (hostRadio == null || hostRadio.size == 32)) {
            "mesh keys are 32 bytes each"
        }
    }

    override fun equals(other: Any?) =
        other is MeshKeyMaterial && hostKey.contentEquals(other.hostKey) && psk.contentEquals(other.psk) &&
            (hostRadio?.contentEquals(other.hostRadio) ?: (other.hostRadio == null)) &&
            hostPreset == other.hostPreset

    // The tuple is part of the value: two links that name different operating points are not the
    // same key material, even though the secrets are identical.
    override fun hashCode() = 31 * (31 * (31 * hostKey.contentHashCode() + psk.contentHashCode()) +
        (hostRadio?.contentHashCode() ?: 0)) + (hostPreset?.hashCode() ?: 0)
    override fun toString() = "MeshKeyMaterial(..)"
}

const val PAIRING_VERSION = 1
/** The mesh-key part's own version (`mv`). */
const val MESH_KEYS_VERSION = 1
private val HEX32 = Regex("^[0-9a-f]{64}$")
private val TOKEN = Regex("^[0-9a-f]{64}$")
private const val NAME_MAX = 64

/**
 * The **tailnet** pairing in [link], or null if it is not a valid ldmux pairing link. A mesh-only
 * link is not one (it has no daemon to pair), so this returns null for it, exactly as Rust's
 * `parse_pairing_link` does: every caller of this function can keep assuming `host` is an address.
 */
fun parsePairingLink(link: String): Pairing? = parseScannedPairing(link)?.takeIf { !it.isMeshOnly }

/**
 * Any pairing link a person may scan: a tailnet pairing (with or without mesh keys) or a
 * mesh-only link. The mirror of Rust's `parse_pairing_link_full`.
 */
fun parseScannedPairing(link: String): Pairing? {
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
    // Mesh keys are all or nothing. A link that carries any of them and gets one wrong is refused
    // whole: a half-formed secret is never quietly dropped, and never downgraded to tailnet-only.
    val mesh: MeshKeyMaterial? = if ("mv" !in params && "mk" !in params && "psk" !in params && "mr" !in params) {
        null
    } else {
        if (params["mv"] != MESH_KEYS_VERSION.toString()) return null
        val mk = params["mk"] ?: return null
        val psk = params["psk"] ?: return null
        if (!HEX32.matches(mk) || !HEX32.matches(psk)) return null
        val mr = params["mr"]
        if (mr != null && !HEX32.matches(mr)) return null
        // A malformed preset is not a reason to refuse the keys: it is one optional sentence about
        // the host's radio, and the code is usable without it.
        val mp = params["mp"]
        MeshKeyMaterial(unhex(mk), unhex(psk), mr?.let(::unhex), mp?.let(::parseHostPreset))
    }
    val p = Pairing(host = params["host"] ?: "", token = params["token"] ?: "", name = params["name"] ?: "", mesh = mesh)
    // Neither host nor token is a mesh-only link, and only valid if the mesh keys were present
    // (parsed above). One of the two alone, or neither with no keys, is refused.
    if ("host" !in params && "token" !in params) {
        return if (mesh != null && nameOk(p.name)) p else null
    }
    return if (pairingProblem(p) == null) p else null
}

/**
 * `freq_khz,bw_hz,sf,cr`, or null when it is not four numbers inside the firmware's bounds.
 *
 * Bounds are the radio's own (`150000..2500000` kHz, `7000..500000` Hz, SF 5..12, CR 5..8): a code
 * that asks for a tuple the firmware would refuse is not a tuple this app should apply.
 */
internal fun parseHostPreset(text: String): RadioPreset? {
    val n = text.split(',').map { it.trim() }
    // Four numbers, or five when the host also said its path hash width in bytes. A host that
    // predates the fifth cannot have one, and its channel is ldmux's own two-byte default.
    if (n.size != 4 && n.size != 5) return null
    val freqKhz = n[0].toIntOrNull() ?: return null
    val bwHz = n[1].toIntOrNull() ?: return null
    val sf = n[2].toIntOrNull() ?: return null
    val cr = n[3].toIntOrNull() ?: return null
    if (freqKhz !in 150_000..2_500_000) return null
    if (bwHz !in 7_000..500_000) return null
    if (sf !in 5..12 || cr !in 5..8) return null
    val path = n.getOrNull(4)?.let { it.toIntOrNull() ?: return null }
    if (path != null && path !in 1..3) return null
    return RadioPreset(
        frequencyMhz = freqKhz / 1000.0,
        bandwidthKhz = bwHz / 1000.0,
        spreadingFactor = sf,
        codingRate = cr,
        txPowerDbm = 22,
        pathHashSize = path ?: 2,
    )
}

private fun unhex(s: String) = ByteArray(s.length / 2) { s.substring(it * 2, it * 2 + 2).toInt(16).toByte() }

/**
 * What the confirmation dialog shows, with nothing hidden by the machine's own name.
 *
 * The link's `name` is chosen by whoever printed the QR code, so it is untrusted
 * text in the one dialog that decides where every keystroke goes. The dialog used
 * to show the name *instead of* the host when there was one, so a code could say
 * "work laptop" and point anywhere on the tailnet. Now the address is always shown,
 * the name is only a label next to it, and a short fingerprint of the token lets the
 * user compare against `ldmux code`'s own output.
 */
data class PairingPrompt(
    /** The name with bidi, format and control characters removed; empty when nothing is left. */
    val name: String,
    /** `host:port`, exactly as it will be connected to. */
    val address: String,
    /** First and last four hex digits of the token, e.g. `3fa9…c01d`. */
    val fingerprint: String,
)

fun pairingPrompt(p: Pairing): PairingPrompt =
    if (p.isMeshOnly) {
        // No daemon address and no token exist. What the user can compare against `ldmux code
        // --mesh-only` on the host is the daemon's public session key, so that is the fingerprint.
        PairingPrompt(
            name = sanitizeDisplayName(p.name),
            address = "mesh route only (no tailnet address)",
            fingerprint = tokenFingerprint(p.mesh!!.hostKey.joinToString("") { (it.toInt() and 0xff).toString(16).padStart(2, '0') }),
        )
    } else {
        PairingPrompt(
            name = sanitizeDisplayName(p.name),
            address = p.host,
            fingerprint = tokenFingerprint(p.token),
        )
    }

/**
 * A name made safe to draw next to an address.
 *
 * Removes every control character (C0, DEL, C1), every format character (zero-width
 * joiners and spaces, the bidirectional marks and the embedding/override/isolate
 * controls U+202A-U+202E and U+2066-U+2069 that can reorder the text around them),
 * unassigned and private-use code points, and turns the line and paragraph separators
 * into spaces. Runs of whitespace collapse to one space and the result is trimmed and capped to
 * the same length the link allows, so one long name cannot push the address out of view.
 */
fun sanitizeDisplayName(name: String, max: Int = NAME_MAX): String {
    val kept = StringBuilder()
    var lastSpace = true
    for (c in name) {
        // Anything that reads as a space (tab, newline, NBSP, the line and paragraph
        // separators) is one space, so words do not fuse when a separator is removed.
        if (c.isWhitespace()) {
            if (!lastSpace) kept.append(' ')
            lastSpace = true
            continue
        }
        val drop = when (c.category) {
            CharCategory.CONTROL,
            CharCategory.FORMAT,
            CharCategory.LINE_SEPARATOR,
            CharCategory.PARAGRAPH_SEPARATOR,
            CharCategory.PRIVATE_USE,
            CharCategory.UNASSIGNED -> true
            else -> false
        }
        if (drop) continue
        kept.append(c)
        lastSpace = false
    }
    val out = kept.toString().trim().take(max)
    // Never end on half of a surrogate pair.
    return if (out.isNotEmpty() && out.last().isHighSurrogate()) out.dropLast(1) else out
}

/** `3fa9…c01d`: enough to compare by eye, far too little to be the secret. */
fun tokenFingerprint(token: String): String =
    if (token.length < 8) "" else token.take(4).lowercase() + "\u2026" + token.takeLast(4).lowercase()

private fun nameOk(name: String) = name.length <= NAME_MAX && name.none { it.code < 0x20 || it.code == 0x7f }

private fun pairingProblem(p: Pairing): String? {
    val hp = splitHost(p.host) ?: return "host"
    if (!isTailnetOrLoopback(hp.first)) return "host"
    if (!TOKEN.matches(p.token)) return "token"
    if (p.name.length > NAME_MAX || p.name.any { it.code < 0x20 || it.code == 0x7f }) return "name"
    return null
}

/** Whether [hostPort] (`address:port`) is a literal tailnet or loopback address. */
fun isAllowedEndpointHost(hostPort: String): Boolean =
    splitHost(hostPort)?.let { isTailnetOrLoopback(it.first) } == true

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

// ── Tailnet only, as data (the mirror of `crates/protocol/src/net.rs`) ───────────

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
