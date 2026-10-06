package dev.ldmux.mobile

/**
 * Everything the app remembers: the pairing, and the few display settings.
 *
 * Its own file because it is the one value every part of the app reads, and it was
 * a hundred lines in the middle of the 1800-line UI file, where the settings dialog
 * rebuilt it by position and lost two fields that way once.
 */
data class LdmuxConfig(
    /**
     * The daemon, as `address:port` — exactly the `host` of the pairing link
     * (ADR-0017). Empty when unpaired.
     */
    val host: String,
    /** The bearer token from the pairing link. Empty when unpaired. */
    val token: String,
    val side: String = "left",
    /**
     * Show typed text before the server echoes it (docs/PREDICTIVE-ECHO.md).
     * Off by default: it trades a little honesty for a lot of latency, and a
     * prompt that does not echo is exactly where the trade is most visible.
     */
    val predict: Boolean = false,
    /**
     * Terminal font size. It is a client setting because the client owns the
     * geometry: a bigger font means fewer cells fit, and the client tells the
     * daemon the grid it can paint, so the pane reflows to match instead of
     * being scaled down to fit (ADR-0008).
     */
    val fontSp: Float = DEFAULT_FONT_SP,
    /**
     * Thin chrome, and this is the default.
     *
     * Every control in the app has one height, and that height is a real decision with
     * two defensible answers, so it is the user's rather than mine. Thin by default: a
     * terminal is a thing you read, and a row of chrome is a row of output you cannot
     * see. Set it false and every control gets the platform's 48dp minimum target,
     * which is what a thumb wants.
     */
    val compact: Boolean = true,
    /**
     * This install's identity, announced with `hello` (ADR-0010).
     *
     * Stable for the life of the install, generated once and kept. It is **not**
     * an account and not a credential: it exists so two devices can be told apart
     * in a sentence about a shell they both want ("open on Pixel 8").
     */
    val deviceId: String = "",
    /** What that sentence calls this device. */
    val deviceLabel: String = "",
    /**
     * What the paired machine is called (the link's `name`), for "paired with
     * debian". Empty for a pairing made before names existed: [pairedName] then
     * falls back to the host.
     */
    val pairName: String = "",
) {
    /**
     * Paired means there is somewhere to connect and something to connect with — and
     * that the place is one this app may speak cleartext to.
     *
     * The transport is plain `ws://` (WireGuard is the encryption, ADR-0016), and
     * Android's network security config cannot say "100.64.0.0/10 only" (it matches
     * domains, never address ranges), so cleartext is permitted app-wide there and
     * **this is the gate**: the only sockets the app opens are built from [endpoint],
     * and [endpoint] exists only for a tailnet or loopback literal — the same rule a
     * pairing link is held to (`isTailnetOrLoopback`). A host saved by an older version
     * that fails it reads as unpaired instead of sending a bearer token across the
     * open internet.
     */
    val isPaired: Boolean get() = host.isNotEmpty() && token.isNotEmpty() && isAllowedEndpointHost(host)

    /** What the app calls the machine it is paired with. */
    val pairedName: String get() = pairName.ifEmpty { host }

    /** How to reach the daemon, or null when unpaired. */
    val endpoint: Endpoint? get() = if (isPaired) Endpoint(host, token) else null

    val panelLeft: Boolean get() = side != "right"

    /** This config paired with [p]: the pairing replaced, everything else kept. */
    fun paired(p: Pairing): LdmuxConfig = copy(host = p.host, token = p.token, pairName = p.name)

    /** This config with the pairing forgotten, everything else kept. */
    fun unpaired(): LdmuxConfig = copy(host = "", token = "", pairName = "")

    /**
     * The fields a live connection depends on. A terminal page is rebuilt when
     * these change and never for anything else: rebuilding one reconnects it, and
     * a reconnect on the first attach takes the shell over (ADR-0010) — so a font
     * change used to take every open shell back from the desk.
     */
    val connectionKey: String get() = "$host|$token|$deviceId|$deviceLabel"

    companion object {
        const val DEFAULT_FONT_SP = 12f
        const val MIN_FONT_SP = 7f
        const val MAX_FONT_SP = 28f

        val DEFAULT = LdmuxConfig(host = "", token = "")
    }
}

/**
 * Where the daemon is and how to prove we may talk to it.
 *
 * The token is **not** in [wsUrl]. It travels as an `Authorization` header (the
 * daemon accepts both), so the URL — which is what gets logged — carries nothing
 * secret. It used to be `?token=…`, and the connect log wrote the credential into
 * logcat on every attempt.
 */
data class Endpoint(val host: String, val token: String) {
    /** Plain `ws://`: WireGuard is the encryption on the tailnet (ADR-0016). */
    val wsUrl: String get() = "ws://$host/ws"

    /** The same path over plain HTTP, which the daemon answers without upgrading — see [probeLink]. */
    val probeUrl: String get() = "http://$host/ws"

    val authorization: String get() = "Bearer $token"

    override fun toString(): String = "Endpoint($host)"
}

/** Platform persistence for [LdmuxConfig]. */
expect object AppPrefs {
    fun load(): LdmuxConfig
    fun save(config: LdmuxConfig)
}

/**
 * A host as saved by an older version, in the shape a pairing gives.
 *
 * Hosts were typed by hand, as `host[:port]` or a URL (`http://…/`), and the old
 * default was `localhost:7623`. A pairing is always `address:port`, so the scheme
 * and a trailing slash are dropped. The untouched default is not a pairing and
 * becomes empty — without its token it never was one.
 */
fun storedHost(saved: String?): String {
    val h = (saved ?: "").trim().removePrefix("http://").removePrefix("https://")
        .removePrefix("ws://").removePrefix("wss://").trimEnd('/')
    return if (h == "localhost:7623") "" else h
}
