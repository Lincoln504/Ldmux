package io.ldmux.mobile

import android.media.AudioAttributes
import android.media.SoundPool

/**
 * The session cues, played through one `SoundPool`.
 *
 * **`SoundPool` and not `MediaPlayer`.** These are 80-400 ms tones played at a moment that matters,
 * possibly while a session is loading; `SoundPool` decodes once into memory and plays a preloaded
 * sample in a millisecond, where `MediaPlayer` prepares a stream per play and can take longer than
 * the sound lasts. It is also the platform's own answered question about latency.
 *
 * A cue is a *session* sound, so it rides `STREAM_MUSIC` rather than the notification stream: a
 * phone on silent has silent notifications, and an operator who turned the app's sounds on meant the
 * media volume they set. It never plays while the app is not in the foreground: a session that
 * reconnects behind a pocket should be silent, and `foreground` is written by the activity's
 * lifecycle.
 */
internal actual object CuePlayer {
    /**
     * Only the foreground app speaks.
     *
     * `AppLifecycle.foreground` is the platform's own answer (written from `onStart`/`onStop`), so
     * a session that rebuilds behind a pocket is silent: a tone nobody is there to hear is a tone
     * that wakes the room.
     */
    private val foreground: Boolean get() = AppLifecycle.foreground

    private const val MAX_STREAMS = 4

    /** Resolved once, on the first cue: a `SoundPool` costs a thread whether or not it is used. */
    private val pool: SoundPool by lazy {
        SoundPool.Builder()
            .setMaxStreams(MAX_STREAMS)
            .setAudioAttributes(
                AudioAttributes.Builder()
                    .setUsage(AudioAttributes.USAGE_MEDIA)
                    .setContentType(AudioAttributes.CONTENT_TYPE_SONIFICATION)
                    .build(),
            )
            .build()
    }

    /** Sample ids per (theme, cue), loaded on demand and kept. */
    private val loaded = HashMap<Int, Int>()

    actual fun play(theme: LdmuxTheme, cue: Cue) {
        if (!foreground) return
        val res = cueResources(theme)[cue] ?: return
        val id = synchronized(loaded) {
            loaded[res] ?: pool.load(AndroidApp.context, res, 1).also { loaded[res] = it }
        }
        // A freshly loaded sample answers -1 or 0 for its first few milliseconds; the platform
        // queues nothing for that id and the play is silently dropped. That is the right failure for
        // a cue: dropping it is better than holding the caller for a load.
        //
        // **Said into the app log, because a sound that does not play is otherwise invisible.** The
        // stream id is 0 when the platform refused (still loading, or the pool is saturated), so
        // "the theme is silent" and "the phone is on silent" stop looking the same from the outside.
        val stream = pool.play(id, 1f, 1f, 1, 0, 1f)
        appLog("cue $theme/${cue.name} → stream $stream (sample $id)")
    }
}

/**
 * The cue table, one entry per (theme, cue) — **exhaustive over both enums**.
 *
 * A new theme or a new cue is a compile error here rather than a silent no-op, and Dark's absence is
 * a value ("no voice") rather than a branch that happens to return nothing. Internal so a test can
 * hold the table to that: every sounding theme has all three, and no two of its cues are the same
 * file.
 */
internal fun cueResources(theme: LdmuxTheme): Map<Cue, Int> = when (theme) {
    LdmuxTheme.Dark -> emptyMap()
    LdmuxTheme.Forest -> mapOf(
        Cue.LinkUp to R.raw.forest_link_up,
        Cue.LinkDown to R.raw.forest_link_down,
        Cue.Attention to R.raw.forest_attention,
    )
    LdmuxTheme.Air -> mapOf(
        Cue.LinkUp to R.raw.air_link_up,
        Cue.LinkDown to R.raw.air_link_down,
        Cue.Attention to R.raw.air_attention,
    )
    LdmuxTheme.Daybreak -> mapOf(
        Cue.LinkUp to R.raw.daybreak_link_up,
        Cue.LinkDown to R.raw.daybreak_link_down,
        Cue.Attention to R.raw.daybreak_attention,
    )
    LdmuxTheme.Gruvbox -> mapOf(
        Cue.LinkUp to R.raw.gruvbox_link_up,
        Cue.LinkDown to R.raw.gruvbox_link_down,
        Cue.Attention to R.raw.gruvbox_attention,
    )
    LdmuxTheme.Nerv -> mapOf(
        Cue.LinkUp to R.raw.nerv_link_up,
        Cue.LinkDown to R.raw.nerv_link_down,
        Cue.Attention to R.raw.nerv_attention,
    )
}
