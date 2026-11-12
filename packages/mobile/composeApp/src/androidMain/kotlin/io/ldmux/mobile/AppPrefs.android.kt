package io.ldmux.mobile

import android.annotation.SuppressLint
import android.content.Context
import io.ldmux.mobile.mesh.KeystoreBlob
import io.ldmux.mobile.mesh.MeshInputMode
import io.ldmux.mobile.mesh.meshRouteFromStored

/**
 * The config, persisted in SharedPreferences, and this install's identity.
 *
 * Its own file: it lived inside the 900-line terminal surface, where nothing about
 * it belonged.
 *
 * The token is **encrypted at rest** with the same mechanism the mesh keys already used: one
 * AES-256-GCM blob under a non-exportable Android Keystore key, ciphertext only in this file.
 * It used to be plain text here, which made the strongest credential in the app the weakest
 * stored secret — `MODE_PRIVATE` and `allowBackup="false"` stop other apps and the cloud, not a
 * rooted device or `adb run-as` on a debuggable build. A token written by an older build is
 * read once, re-encrypted, and removed from the plaintext key.
 */
// **The application context, which is the one Context that must outlive everything.** Android's
// lint flags any static Context; this one is `applicationContext`, set once in `Application.onCreate`,
// and it lives exactly as long as the process. It is not an Activity, a Service or a View, so
// nothing here can hold one of those alive.
@SuppressLint("StaticFieldLeak")
object AndroidApp {
    lateinit var context: Context

    /**
     * [context] if it has been set, else null.
     *
     * A `lateinit` is right for code that cannot run before `MainActivity.onCreate`, and wrong for a
     * unit test that builds an engine with its own transport: reading it there threw
     * `UninitializedPropertyAccessException` from five tests at once. This is the check that lets
     * both be true, and it reports null rather than pretending.
     */
    val maybeContext: Context?
        get() = if (this::context.isInitialized) context else null
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
    private const val KEY_TOKEN = "token" // legacy plaintext; migrated on first load
    private const val KEY_TOKEN_BLOB = "tailnet_token"
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
    private const val KEY_MESH_INPUT_MODE = "mesh_input_mode"
    private const val KEY_MESH_LAST_GRID = "mesh_last_grid"
    /** The desktop's tuple from the pairing code: four numbers, never a secret. */
    private const val KEY_MESH_HOST_PRESET = "mesh_host_preset"
    /**
     * Whether a finished shell may post a notification.
     *
     * **It was never written.** The switch existed, `Config` carried it, and neither `load` nor
     * `save` touched it, so it read back as the default (`off`) on every launch: an operator turned
     * notifications on and found them off again the next time the app started. A preference that
     * silently resets is worse than no preference.
     */
    private const val KEY_NOTIFY_MARKS = "notify_marks"
    /** The look, stored by name — never by ordinal (see `LdmuxTheme.fromStored`). */
    private const val KEY_THEME = "theme"
    /** Whether a session event plays the theme's cue. Dark has none, whatever this says. */
    private const val KEY_SOUNDS = "sounds"

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
            token = loadToken(sp),
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
            // Stored by name, not by ordinal: an enum's order is an implementation detail, and a
            // value that moved would silently change the mode of every install that had saved it.
            meshHostPreset = sp.getString(KEY_MESH_HOST_PRESET, null) ?: "",
            notifyMarks = sp.getBoolean(KEY_NOTIFY_MARKS, LdmuxConfig.DEFAULT.notifyMarks),
            theme = LdmuxTheme.fromStored(sp.getString(KEY_THEME, null)),
            sounds = sp.getBoolean(KEY_SOUNDS, LdmuxConfig.DEFAULT.sounds),
            meshInputMode = sp.getString(KEY_MESH_INPUT_MODE, null)
                ?.let { name -> MeshInputMode.entries.firstOrNull { it.name == name } }
                ?: LdmuxConfig.DEFAULT.meshInputMode,
            meshLastGrid = sp.getString(KEY_MESH_LAST_GRID, LdmuxConfig.DEFAULT.meshLastGrid) ?: "",
        )
    }

    /** The token, from the Keystore blob, migrating a plaintext one from an older build. */
    private fun loadToken(sp: android.content.SharedPreferences): String {
        tokenBlob.load()?.let { return String(it, Charsets.UTF_8) }
        val legacy = sp.getString(KEY_TOKEN, null) ?: return ""
        if (legacy.isEmpty()) return ""
        // Keep the plaintext until the encrypted copy is actually written: a Keystore that
        // refuses would otherwise lose the pairing on the next launch.
        val stored = runCatching { tokenBlob.save(legacy.toByteArray(Charsets.UTF_8)) }.isSuccess
        if (stored) sp.edit().remove(KEY_TOKEN).apply()
        return legacy
    }

    private val tokenBlob by lazy { KeystoreBlob(AndroidApp.context, KEY_TOKEN_BLOB, FILE) }

    actual fun save(config: LdmuxConfig) {
        AndroidApp.context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
            .edit()
            .putString(KEY_HOST, config.host)
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
            .putBoolean(KEY_NOTIFY_MARKS, config.notifyMarks)
            .putString(KEY_THEME, config.theme.name)
            .putBoolean(KEY_SOUNDS, config.sounds)
            .putString(KEY_MESH_INPUT_MODE, config.meshInputMode.name)
            .putString(KEY_MESH_LAST_GRID, config.meshLastGrid)
            .putString(KEY_MESH_HOST_PRESET, config.meshHostPreset)
            .apply()
        // **The token is rewritten only when it changed.** Every settings save used to re-encrypt it
        // through the Keystore, and the text-size stepper saves on every step — holding the button
        // meant an encrypt per 90 ms on the composition dispatcher. The token is a credential that
        // changes when a pairing does, which is the only time it needs writing.
        val current = runCatching { tokenBlob.load()?.let { String(it, Charsets.UTF_8) } }.getOrNull()
        if (current == config.token) return
        // Encrypt BEFORE dropping the plaintext. If the Keystore refuses — a restored backup, a
        // reset keystore — keeping the old plaintext key is the difference between a pairing that
        // still works and one the user has to redo. The plaintext is removed only on success.
        val encrypted = runCatching {
            tokenBlob.save(config.token.toByteArray(Charsets.UTF_8))
        }.isSuccess
        if (encrypted) {
            AndroidApp.context.getSharedPreferences(FILE, Context.MODE_PRIVATE)
                .edit()
                .remove(KEY_TOKEN)
                .apply()
        }
    }
}
