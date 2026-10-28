package dev.ldmux.mobile.voice

import dev.ldmux.mobile.flag
import dev.ldmux.mobile.optionalText
import dev.ldmux.mobile.parseJsonObject
import dev.ldmux.mobile.text

/**
 * One thing an agent wants said aloud. Both producers end here: the daemon's `speech` message
 * (Channel A, how pi is heard) and a private OSC sequence in the byte stream (Channel B, how
 * any other program is heard). One handler downstream, no knowledge of which it came from.
 *
 * The text is untrusted program output. It is spoken and never executed or typed back.
 */
data class SpeechEvent(
    val text: String,
    /** The sender's id for this message, if it gave one. */
    val id: String? = null,
    /** True when nothing more follows for this turn, so the microphone may reopen. */
    val final: Boolean = true,
)

/** The OSC number carrying speech. Not claimed by the engine: see [SpeechOscParser]. */
const val SPEECH_OSC_NUMBER: String = "1338"

/** The most payload bytes accepted between `ESC ] 1338 ;` and the terminator. */
const val SPEECH_OSC_MAX_BYTES: Int = 4096

private const val ESC: Int = 0x1B
private const val BEL: Int = 0x07
private const val MAX_ID_BYTES: Int = 64

/**
 * Channel B: `ESC ] 1338 ; {"v":1,"kind":"say","text":"...","id":"...","final":true} BEL`
 * (or `ESC \` as the terminator), read from the raw bytes **before** they reach the emulator.
 *
 * **Why OSC 1338, and why a parser here.** The engine's own unknown-sequence callback reports
 * only APC (`terminal.h:345-356`), and the daemon drops every DCS/SOS/PM/APC string whole
 * (`crates/daemon/src/sanitize.rs`), so APC is not available. An OSC passes the sanitizer, and
 * the engine's OSC parser, at the pinned commit `d5eba8d`, reaches state `@"133"` after `1;3;3`
 * and then marks any character other than `;` or `7` invalid (`src/terminal/osc.zig`, the
 * `.@"133"` arm): 1338 is not a command the engine knows, so it is consumed and ignored, and the
 * bytes can safely still be passed on to it.
 *
 * This parser **observes**; it never removes bytes from the stream.
 *
 * Streaming: a sequence may be split across any number of `feed` calls. Rules:
 *  - a payload over [SPEECH_OSC_MAX_BYTES] is dropped whole, never truncated into something
 *    that gets spoken;
 *  - JSON that is malformed, a `v` other than 1, a `kind` other than `say`, an empty `text`,
 *    or an `id` over 64 bytes is dropped;
 *  - any OSC other than 1338 is skipped without buffering it.
 */
class SpeechOscParser {
    private enum class S { Ground, Esc, Number, Payload, PayloadEsc, Skip, SkipEsc }

    private var state = S.Ground
    private val number = StringBuilder()
    private var payload = ByteArray(256)
    private var length = 0
    private var overflow = false

    /** Feed `length` bytes of `bytes`; returns every speech event completed by them. */
    fun feed(bytes: ByteArray, length: Int = bytes.size): List<SpeechEvent> {
        var out: MutableList<SpeechEvent>? = null
        for (i in 0 until length) {
            val b = bytes[i].toInt() and 0xFF
            when (state) {
                S.Ground -> if (b == ESC) state = S.Esc
                S.Esc -> state = when (b) {
                    ']'.code -> { number.setLength(0); S.Number }
                    ESC -> S.Esc
                    else -> S.Ground
                }
                S.Number -> when {
                    b in '0'.code..'9'.code && number.length < 5 -> number.append(b.toChar())
                    b == ';'.code -> {
                        if (number.toString() == SPEECH_OSC_NUMBER) {
                            this.length = 0
                            overflow = false
                            state = S.Payload
                        } else {
                            state = S.Skip
                        }
                    }
                    b == BEL -> state = S.Ground
                    b == ESC -> state = S.SkipEsc
                    else -> state = S.Skip
                }
                S.Skip -> when (b) {
                    BEL -> state = S.Ground
                    ESC -> state = S.SkipEsc
                }
                S.SkipEsc -> state = when (b) {
                    '\\'.code -> S.Ground
                    ']'.code -> { number.setLength(0); S.Number }
                    else -> S.Skip
                }
                S.Payload -> when (b) {
                    BEL -> {
                        finish()?.let { (out ?: mutableListOf<SpeechEvent>().also { l -> out = l }).add(it) }
                        state = S.Ground
                    }
                    ESC -> state = S.PayloadEsc
                    else -> append(b)
                }
                S.PayloadEsc -> when (b) {
                    '\\'.code -> {
                        finish()?.let { (out ?: mutableListOf<SpeechEvent>().also { l -> out = l }).add(it) }
                        state = S.Ground
                    }
                    ']'.code -> { number.setLength(0); state = S.Number }
                    else -> state = S.Ground // an ESC inside the payload: not ours, abandon it
                }
            }
        }
        return out ?: emptyList()
    }

    private fun append(b: Int) {
        if (overflow) return
        if (length >= SPEECH_OSC_MAX_BYTES) {
            overflow = true
            return
        }
        if (length == payload.size) payload = payload.copyOf(minOf(payload.size * 2, SPEECH_OSC_MAX_BYTES))
        payload[length++] = b.toByte()
    }

    private fun finish(): SpeechEvent? {
        if (overflow) return null
        return parseSpeechPayload(payload.decodeToString(0, length))
    }

    /** Back to the start, for a session switch: a half-read sequence must not complete later. */
    fun reset() {
        state = S.Ground
        length = 0
        overflow = false
    }
}

/**
 * The versioned payload, shared with the daemon's typed message. Null when it is anything
 * other than a well-formed `v:1` `say`.
 */
fun parseSpeechPayload(json: String): SpeechEvent? {
    val root = parseJsonObject(json) ?: return null
    if (root.text("v") != "1" || root.text("kind") != "say") return null
    val text = root.text("text")
    if (text.isBlank()) return null
    val id = root.optionalText("id")?.takeIf { it.isNotEmpty() }
    if (id != null && id.encodeToByteArray().size > MAX_ID_BYTES) return null
    val final = if (root["final"] == null) true else root.flag("final")
    return SpeechEvent(text = text, id = id, final = final)
}
