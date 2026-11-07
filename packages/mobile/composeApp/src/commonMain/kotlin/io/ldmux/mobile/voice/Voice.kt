package io.ldmux.mobile.voice

import io.ldmux.mobile.LocalTouch

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import io.ldmux.mobile.Accent
import io.ldmux.mobile.Surface
import io.ldmux.mobile.MonoFont

/** What the voice button is doing, for the little on-screen indicator. */
sealed interface VoiceState {
    data object Idle : VoiceState
    data object RequestingPermission : VoiceState
    data object Listening : VoiceState

    /**
     * Listening, but the platform could not do it on the device: the audio goes to a
     * cloud recogniser. Said on screen, because dictated shell commands (and anything
     * said near the phone) leaving the device is something the user should know about
     * before they speak, not discover.
     */
    data object ListeningCloud : VoiceState

    /** [cloud]: the words are being recognised off the device. */
    data class Partial(val text: String, val cloud: Boolean = false) : VoiceState
    data object Denied : VoiceState

    /**
     * The recogniser ended with nothing usable (a timeout, or no match). One-shot dictation
     * shows it; continuous voice mode treats it as silence and listens again.
     */
    data object NoSpeech : VoiceState
    data object Unavailable : VoiceState
    data class Failed(val reason: String) : VoiceState
}

/**
 * Headless dictation, so voice can be a first-class input without the on-screen
 * keyboard ever appearing.
 *
 * This deliberately does **not** go through the IME's voice typing — the microphone
 * on the soft keyboard. That path only exists while a keyboard is up and an
 * `InputConnection` is live, which is exactly what a terminal wants to avoid. The
 * platform's native recogniser is a headless service: no keyboard, no overlay, and
 * results we forward straight into the pane.
 *
 * **The seam and its reason (ADR-0013).** The concept is "recognise speech and hand
 * me the text", and that is all this says. It deliberately does not take an
 * Activity: reaching a recogniser through one is Android's way of doing it, not part
 * of what dictation *is*, and a seam shaped like the first platform's API is a seam
 * the second platform has to lie about. iOS has `SFSpeechRecognizer` and needs no
 * such handle, so the Android side obtains its own and nothing here mentions it.
 *
 * It is an `object` with one microphone behind it rather than one per screen. That
 * is a correction, not a preference: it used to be driven from each terminal
 * surface, of which the pager keeps a neighbour composed for an instant swipe — so
 * disposing one page stopped dictation that belonged to another, and two composed
 * pages could each draw the same notice.
 */
expect object Dictation {
    /** True while a recognition session is running (the button toggles it). */
    val listening: Boolean

    /**
     * Start one dictation, or stop the one running.
     *
     * Results arrive through [onState]: partials while the user speaks, then
     * [VoiceState.Idle] when the session ends, and the final transcript through
     * `onText` before that.
     */
    fun toggle(onText: (String) -> Unit, onState: (VoiceState) -> Unit)

    fun stop()
}

/** What the microphone is doing, drawn over the terminal it will type into. */
@Composable
internal fun VoiceBanner(state: VoiceState, modifier: Modifier) {
    val text = when (state) {
        VoiceState.Idle -> return
        VoiceState.RequestingPermission -> "microphone permission…"
        VoiceState.Listening -> "listening…"
        VoiceState.ListeningCloud -> "listening · cloud, audio leaves phone"
        VoiceState.Denied -> "microphone permission denied"
        VoiceState.NoSpeech -> "no speech heard"
        VoiceState.Unavailable -> "no speech recognition here"
        is VoiceState.Partial -> if (state.cloud) "\u2601 ${state.text}" else state.text
        is VoiceState.Failed -> "voice: ${state.reason}"
    }
    // The one place a hue is allowed, because these are the states a user has to
    // act on rather than a decoration: desaturated, but unmistakably not grey.
    val accent = when (state) {
        is VoiceState.Failed, VoiceState.Denied, VoiceState.Unavailable -> Color(0xFFFF6B6B)
        else -> Accent
    }
    Box(
        modifier
            .padding(8.dp)
            .background(Surface)
            .padding(horizontal = 10.dp, vertical = LocalTouch.current.rowPad),
    ) {
        Text(text, color = accent, fontSize = 12.sp, fontFamily = MonoFont)
    }
}
