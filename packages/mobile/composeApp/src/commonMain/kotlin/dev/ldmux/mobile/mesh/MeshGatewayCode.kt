package dev.ldmux.mobile.mesh

import dev.ldmux.mobile.splitHost

/**
 * Where MeshMore's gateway is listening, on this phone.
 *
 * The gateway is a **loopback** service (`GatewayServer`, `127.0.0.1`), so the host here is a
 * loopback literal and nothing else: a code that names another host would send the L2 token off
 * the device, and is refused. The port is data, not a constant in the phone alone, because
 * MeshMore may be run on another one (`meshmore_gateway --listen`).
 */
data class MeshGatewayEndpoint(val host: String, val port: Int) {
    override fun toString() = if (host.contains(':')) "[$host]:$port" else "$host:$port"

    companion object {
        val DEFAULT = MeshGatewayEndpoint("127.0.0.1", MeshGateway.DEFAULT_PORT)

        /** `host:port` with a loopback host, or null. */
        fun parse(text: String): MeshGatewayEndpoint? {
            val hp = splitHost(text.trim()) ?: return null
            val host = hp.first.removePrefix("[").removeSuffix("]")
            return if (isLoopbackLiteral(host)) MeshGatewayEndpoint(host, hp.second) else null
        }
    }
}

/** Loopback only: `127.0.0.0/8`, `::1`, `localhost`. Not the tailnet: the gateway is on this device. */
internal fun isLoopbackLiteral(host: String): Boolean {
    val h = host.trim().lowercase()
    if (h == "localhost" || h == "::1") return true
    val parts = h.split('.')
    if (parts.size != 4 || parts[0] != "127") return false
    return parts.all { p -> p.isNotEmpty() && p.length <= 3 && p.all(Char::isDigit) && (p.length == 1 || p[0] != '0') && p.toInt() <= 255 }
}

/**
 * What MeshMore's "Connected apps" QR carries: the L2 grant, and only that.
 *
 *     meshmore://gateway?v=1&host=127.0.0.1:41731&token=<32 hex>
 *
 * It holds no key material and no session secret (the L3 keys arrive with the ldmux pairing code),
 * so a photographed code lets someone talk to the radio gateway on this phone and nothing else.
 * A bare 32-hex token, which is what earlier MeshMore builds printed, is still read, as "the
 * default endpoint". The token is a credential: never in [toString].
 *
 * Mirror of `GatewayPairingPayload` in `meshcore_gateway` (MeshMore). The two repositories pin the
 * same vectors in their own tests; change one and change the other.
 */
class MeshGatewayCode(val endpoint: MeshGatewayEndpoint, val token: ByteArray) {
    override fun equals(other: Any?) = other is MeshGatewayCode && endpoint == other.endpoint && token.contentEquals(other.token)
    override fun hashCode() = 31 * endpoint.hashCode() + token.contentHashCode()
    override fun toString() = "MeshGatewayCode($endpoint)"
}

const val MESH_GATEWAY_CODE_VERSION = 1
private val TOKEN_HEX = Regex("^[0-9a-f]{32}$")

/** The gateway grant in [text], or null if it is not one. Anything malformed is refused whole. */
fun parseGatewayCode(text: String): MeshGatewayCode? {
    val s = text.trim()
    if (TOKEN_HEX.matches(s)) return MeshGatewayCode(MeshGatewayEndpoint.DEFAULT, hex(s))
    val prefix = "meshmore://gateway"
    if (!s.startsWith(prefix, ignoreCase = true)) return null
    val rest = s.substring(prefix.length).substringBefore('#')
    if (!rest.startsWith("?")) return null
    val params = HashMap<String, String>()
    for (part in rest.substring(1).split('&')) {
        if (part.isEmpty() || '=' !in part) continue
        val k = part.substringBefore('=')
        if (k !in params) params[k] = part.substringAfter('=')
    }
    if (params["v"] != MESH_GATEWAY_CODE_VERSION.toString()) return null
    val token = params["token"]?.takeIf { TOKEN_HEX.matches(it) } ?: return null
    val endpoint = params["host"]?.let { MeshGatewayEndpoint.parse(it) ?: return null } ?: MeshGatewayEndpoint.DEFAULT
    return MeshGatewayCode(endpoint, hex(token))
}

private fun hex(s: String) = ByteArray(s.length / 2) { s.substring(it * 2, it * 2 + 2).toInt(16).toByte() }
