package dev.ldmux.mobile

import android.content.Context
import dev.ldmux.mobile.mesh.meshRouteFromStored

/**
 * The config, persisted in SharedPreferences, and this install's identity.
 *
 * Its own file: it lived inside the 900-line terminal surface, where nothing about
 * it belonged.
 *
 * The token is stored the way it always was, in the app's private preferences
 * (`allowBackup="false"`, so it is not copied off the device by a backup). It is
 * the same secret `ldmux code` shows on screen; a hardware-backed keystore would
 * guard it from a rooted device, and is the next step if that matters.
 */
object AndroidApp {
    lateinit var context: Context
}

/**
 * A random install id, generated once.
 *
 * `UUID.randomUUID` rather than a hardware identifier: this must not be a
 * tracking id, and it must not be spoofable into someone else's — it only has to
 * be unique among the handful of devices that share a daemon.
 */
actual fun newDeviceId(): String = java.util.UUID.randomUUID().toString()

/** The model name, e.g. "Pixel 8" — what "open on …" should call this phone. */
actual fun deviceLabel(): String = android.os.Build.MODEL.ifEmpty { "phone" }

actual object AppPrefs {
    private const val FILE = "ldmux"
    private const val KEY_HOST = "host"
    private const val KEY_TOKEN = "token"
    private const val KEY_PAIR_NAME = "pair_name"
    private const val KEY_SIDE = "side"
    private const val KEY_FONT = "font_sp"
    private const val KEY_COMPACT = "compact"
    private const val KEY_DEVICE_ID = "device_id"
    private const val KEY_DEVICE_LABEL = "device_label"
    private const val KEY_ROUTE = "route"
    private const val KEY_MESH_RADIO = "mesh_radio"
    private const val KEY_MESH_RADIO_NAME = "mesh_radio_name"
    private const val KEY_MESH_OPEN_AIRTIME = "mesh_open_airtime"
    private const val KEY_MESH_PUBLIC_CHANNEL = "mesh_public_channel"

    actual fun load(): LdmuxConfig {
        val sp = AndroidApp.context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
        // Generated on first load and written back immediately, so the identity
        // is stable from the first connection onwards — including the very first
        // one, which is the one that creates the claim this id is attributed to.
        val deviceId = sp.getString(KEY_DEVICE_ID, null)
            ?.takeIf { it.isNotEmpty() }
            ?: newDeviceId().also { sp.edit().putString(KEY_DEVICE_ID, it).apply() }
        val label = sp.getString(KEY_DEVICE_LABEL, null)
            ?.takeIf { it.isNotEmpty() }
            ?: deviceLabel().also { sp.edit().putString(KEY_DEVICE_LABEL, it).apply() }
        return LdmuxConfig(
            host = storedHost(sp.getString(KEY_HOST, null)),
            token = sp.getString(KEY_TOKEN, null) ?: "",
            pairName = sp.getString(KEY_PAIR_NAME, null) ?: "",
            side = sp.getString(KEY_SIDE, null) ?: LdmuxConfig.DEFAULT.side,
            fontSp = sp.getFloat(KEY_FONT, LdmuxConfig.DEFAULT_FONT_SP)
                .coerceIn(LdmuxConfig.MIN_FONT_SP, LdmuxConfig.MAX_FONT_SP),
            compact = sp.getBoolean(KEY_COMPACT, LdmuxConfig.DEFAULT.compact),
            deviceId = deviceId,
            deviceLabel = label,
            route = meshRouteFromStored(sp.getString(KEY_ROUTE, null)),
            meshRadio = sp.getString(KEY_MESH_RADIO, null) ?: "",
            meshRadioName = sp.getString(KEY_MESH_RADIO_NAME, null) ?: "",
            meshOpenAirtime = sp.getBoolean(KEY_MESH_OPEN_AIRTIME, true),
            meshPublicChannel = sp.getBoolean(KEY_MESH_PUBLIC_CHANNEL, LdmuxConfig.DEFAULT.meshPublicChannel),
        )
    }

    actual fun save(config: LdmuxConfig) {
        AndroidApp.context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
            .edit()
            .putString(KEY_HOST, config.host)
            .putString(KEY_TOKEN, config.token)
            .putString(KEY_PAIR_NAME, config.pairName)
            .putString(KEY_SIDE, config.side)
            .putFloat(KEY_FONT, config.fontSp)
            .putBoolean(KEY_COMPACT, config.compact)
            .putString(KEY_DEVICE_ID, config.deviceId)
            .putString(KEY_DEVICE_LABEL, config.deviceLabel)
            .putString(KEY_ROUTE, config.route.name)
            .putString(KEY_MESH_RADIO, config.meshRadio)
            .putString(KEY_MESH_RADIO_NAME, config.meshRadioName)
            .putBoolean(KEY_MESH_OPEN_AIRTIME, config.meshOpenAirtime)
            .putBoolean(KEY_MESH_PUBLIC_CHANNEL, config.meshPublicChannel)
            .apply()
    }
}
