package dev.ldmux.mobile.voice

/**
 * Where one shell's speech enters the phone, from either producer: the daemon's typed `speech`
 * message ([deliver]) and an OSC 1338 in the shell's byte stream ([feedBytes]). Both come out of
 * [listener] as the same [SpeechEvent], so nothing downstream can tell which channel carried it.
 *
 * **No listener, no parse.** A shell that voice mode is not bound to has no listener, so its
 * bytes are not even scanned for the sequence: speech from a shell that is not on screen is
 * ignored at the source rather than parsed and then dropped. (This used to live inline in
 * `TerminalController`, which cannot be built in a JVM test, so none of it was tested.)
 *
 * **Spoken once.** A reconnect can replay recent output, and a program can say the same thing on
 * both channels. A message that carries an id is passed on once; one without an id cannot be
 * told from a repeat and is passed on every time.
 */
class SpeechIntake {
    private val osc = SpeechOscParser()
    private val recentIds = ArrayDeque<String>()

    var listener: ((SpeechEvent) -> Unit)? = null
        set(value) {
            field = value
            // A half-read sequence belonged to whoever was listening before.
            osc.reset()
        }

    /** Drop a half-read sequence (the picture it belonged to was replaced). */
    fun resetParser() = osc.reset()

    /** Raw shell output. Cheap when nobody listens. */
    fun feedBytes(bytes: ByteArray, length: Int = bytes.size) {
        if (listener == null) return
        osc.feed(bytes, length).forEach { deliver(it) }
    }

    /** The daemon's typed message (already parsed). */
    fun deliver(e: SpeechEvent) {
        val sink = listener ?: return
        val id = e.id
        if (id != null) {
            if (id in recentIds) return
            recentIds.addLast(id)
            while (recentIds.size > RECENT_IDS) recentIds.removeFirst()
        }
        sink(e)
    }

    private companion object {
        const val RECENT_IDS = 16
    }
}
