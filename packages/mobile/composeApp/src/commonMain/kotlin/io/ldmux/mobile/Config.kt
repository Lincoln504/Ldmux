package io.ldmux.mobile

import io.ldmux.mobile.mesh.MeshInputMode
import io.ldmux.mobile.mesh.MeshRoute
import io.ldmux.mobile.mesh.MeshRouteProblem
import io.ldmux.mobile.mesh.MeshRouteResolution
import io.ldmux.mobile.mesh.resolveRoute

/**
 * Everything the app remembers: the pairings, and the few display settings.
 *
 * **Two pairings live here, and they are different things.** A device may have either, or both.
 *  - The **tailnet pairing** is [host] and [token]: the daemon's address and its bearer token.
 *    [isPaired], [endpoint] and [pairName] are about it, and about nothing else.
 *  - The **mesh pairing** is the host radio's public key plus the L3 session keys, which are
 *    secrets and therefore live in the Keystore (`MeshSessionKeyStore`), not here. The selected
 *    radio's BLE address and label are not secrets. Whether the pairing is complete is a question
 *    about secure storage (`meshSetupView`), never about [host] or [token].
 * [route] says which one a launch uses.
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
     * a hostname". Empty for a pairing made before names existed: [pairedName] then
     * falls back to the host.
     */
    val pairName: String = "",
    /**
     * How this pairing is reached (`docs/ARCHITECTURE.md#2-the-three-modes`). A choice persisted beside the
     * pairing, **not a second credential**: the session keys live in the Keystore and arrive with
     * the pairing ceremony. There is no gateway token. The default is the tailnet, which is every existing
     * install, so a device that never chose mesh behaves exactly as before.
     */
    val route: MeshRoute = MeshRoute.Tailnet,
    /** The BLE address of the phone's MeshCore radio. Not a secret; empty until chosen. */
    val meshRadio: String = "",
    /** The advertised name of that radio, for the Settings row. Not a secret. */
    val meshRadioName: String = "",
    /**
     * Data records are metered at 36 s/hour by default only when this is false.
     * LDMUX defaults to a private tuple, which is not a public MeshCore channel,
     * so the app budget starts open. The radio's own duty cycle still applies.
     */
    val meshOpenAirtime: Boolean = true,
    /**
     * The radio is on a **public channel**: a tuple that other people's radios are also on.
     *
     * It is not a setting the user flips; it is a fact derived from the tuple the radio is
     * actually using, written here by the radio dialog when it reads `SELF_INFO` or applies a
     * preset. It decides one thing: public-channel mode (ADR-0026) is **forced** on a public
     * channel and optional on a private one, because on a shared channel every keystroke is a
     * broadcast and the mode is the only supported way to type there.
     *
     * Defaults to true, because true is the safe reading of "unknown": a fresh install that has
     * not classified its radio has not proved it is on a private tuple, and being wrong towards
     * "held until sync" costs a tap, while being wrong the other way costs a broadcast.
     */
    val meshPublicChannel: Boolean = true,
    /**
     * How typing leaves the phone on the mesh route: as it is typed, or when the user presses sync.
     *
     * **Two modes, chosen in Settings, not in the terminal.** The terminal used to carry the
     * switch as a `hold input` / `leave` pair in its own row, which made a connection-level
     * decision look like a per-screen toggle and put a "leave" button next to the keys. The mode
     * is a property of the link, so it is set where the link is set, beside the channel.
     *
     * [MeshInputMode.Live] is the default: a private channel is the app's own radio talking to
     * the app's own host, and a keystroke that waits for a button is a keystroke the user has to
     * remember to release. [MeshInputMode.ManualSync] is the mode for a shared channel and for a
     * link whose budget is the constraint.
     */
    val meshInputMode: MeshInputMode = MeshInputMode.Live,
    /**
     * The tuple the desktop's radio reported in the pairing code, as `freq_khz,bw_hz,sf,cr`.
     *
     * Stored so the radio panel can say what the desktop is on and offer to match it, days after the
     * scan. Empty when the code carried none (an older daemon, or a host behind a gateway), which
     * reads as "unknown" and shows nothing.
     */
    val meshHostPreset: String = "",
    /**
     * Post a shell's mark as an Android notification, over the tailnet route.
     *
     * **Off by default, and never on the radio route.** The mark in the shell list is the product;
     * a banner is a second way of being told and a terminal that buzzes without being asked for is
     * a terminal that gets uninstalled. The radio route cannot honour this without keeping a radio
     * session alive to receive it, which costs airtime for something the list already says.
     */
    val notifyMarks: Boolean = false,
    /** Whether a mark's notification makes a sound. Picks the sounding channel or the silent one. */
    val notifySound: Boolean = true,
) {

    /**
     * Whether typing is held until the user presses sync.
     *
     * A public channel forces it (ADR-0026): every keystroke there is a broadcast to everyone in
     * range, so held input is the only supported way to type. On a private channel it is the
     * user's choice. This is the one value the terminal reads, so the rule lives in one place
     * rather than in a condition repeated at every call site.
     */
    val meshHoldsInput: Boolean
        get() = meshPublicChannel || meshInputMode == MeshInputMode.ManualSync

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

    /** How to reach the daemon, or null when there is no tailnet pairing **or the route is not the tailnet**. */
    val endpoint: Endpoint? get() = if (isPaired && route == MeshRoute.Tailnet) Endpoint(host, token) else null

    /**
     * Why a paired config has no [endpoint], or null. This is the transport choice: every socket
     * the app opens is built from [endpoint], so a non-tailnet route that cannot run opens
     * **nothing** (it never falls back to the tailnet), and the launcher says why.
     */
    fun routeProblem(
        hasPeer: Boolean = false,
        hasSessionKeys: Boolean = false,
    ): MeshRouteProblem? {
        // Not gated on [isPaired]: that is the tailnet pairing, and a mesh-only device has none.
        if (route == MeshRoute.Tailnet) return null
        return (resolveRoute(route, hasPeer, hasSessionKeys = hasSessionKeys) as? MeshRouteResolution.Unusable)?.problem
    }

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
    val connectionKey: String
        get() = "$host|$token|$deviceId|$deviceLabel" +
            // Appended only off the tailnet, so a tailnet device's key is byte-for-byte what it
            // was and nothing rebuilds on upgrade; choosing a route does rebuild the page.
            if (route == MeshRoute.Tailnet) "" else "|route=${route.name}|radio=$meshRadio|name=$meshRadioName"

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
