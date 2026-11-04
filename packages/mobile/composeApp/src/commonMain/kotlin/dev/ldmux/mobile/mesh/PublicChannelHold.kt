package dev.ldmux.mobile.mesh

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue

/**
 * Public-channel mode: what you type waits on the phone, and the view does not follow the host.
 *
 * On a public channel every byte is broadcast to everyone in range, and the link's own budget is
 * about 58 records an hour (`docs/MESH-GATEWAY.md`, ADR-0025). Neither is a reason for a keystroke
 * to leave as it is typed: a person composing a command wants the command to leave when *they*
 * decide, and the screen to answer then. That decision, and the frozen view that goes with it, is
 * this object.
 *
 * **One button, send and receive.** The link is asynchronous — sending queues records, and the
 * host's answer arrives as the next `Commit` whenever the screen changes — so a "sync" cannot be a
 * request/response in one tap. It is *release, then wait for the answer*: [sync] starts the wait,
 * [acceptScreen] draws the first screen that arrives, and [timedOut] stops a wait that is never
 * answered. A separate "receive" button would only duplicate that wait.
 *
 * **What it is not.** It does not stop the link: the driver keeps polling, the session keeps its
 * acks, and screens keep arriving — they are kept, not drawn. A frozen view is a view, not a
 * disconnected terminal, and the difference is why [updates] is counted and shown.
 *
 * No engine, no link and no clock: [MeshTerminalSource] consults it and the screen reads it, so the
 * rule is tested without either. The fields are Compose state because the bar draws them, and a
 * frozen view that did not tell the bar it was waiting would be a mode with no indicator.
 */
internal class PublicChannelHold {
    /** The mode is engaged. */
    var on: Boolean by mutableStateOf(false)
        private set

    /** A sync is out and its answer has not arrived: the next screen the host sends is drawn. */
    var awaiting: Boolean by mutableStateOf(false)
        private set

    /**
     * The buffer is being drained right now, so the pump may send.
     *
     * Deliberately separate from [awaiting], and the separation is load-bearing: a wait for the
     * answer can last seconds, and typing during it must still be held. A single flag would leak
     * everything typed while the answer was on its way — which is exactly the live sending the
     * mode exists to prevent. The flag clears when the buffer is empty, not when the answer
     * arrives.
     */
    var flushing: Boolean by mutableStateOf(false)
        private set

    /** Screens the host sent while the view was frozen. The newest one is what a pull shows. */
    var updates: Int by mutableStateOf(0)
        private set

    /** Turn the mode on or off. Turning it off clears any wait; the caller draws what it has. */
    fun set(value: Boolean) {
        if (on == value) return
        on = value
        awaiting = false
        flushing = false
        updates = 0
    }

    /**
     * A screen arrived from the host. True means draw it now; false means keep it as the newest
     * held screen.
     *
     * [painted] is whether anything has been drawn yet. The first screen of a session is always
     * drawn, frozen or not: a mode that blanks the terminal before the user has seen anything
     * would be a mode with no reason on screen.
     */
    fun acceptScreen(painted: Boolean): Boolean {
        if (!on || awaiting) return true
        if (!painted) return true
        updates += 1
        return false
    }

    /** A screen was drawn. A wait that was running is over. */
    fun painted() {
        awaiting = false
        flushing = false
        updates = 0
    }

    /**
     * The sync button. True means there is something to send, so the caller must flush the buffer;
     * false means nothing was waiting, and the caller should show the newest screen it holds.
     *
     * A second press while a wait is running is allowed and means "try again": the buffer may have
     * refilled since, and the wait may be on a link that has gone quiet.
     */
    fun sync(pending: Boolean): Boolean {
        if (!on) return false
        awaiting = true
        flushing = pending
        return pending
    }

    /** The buffer is empty: stop sending. Called by the pump's own loop, not by the answer. */
    fun drained() {
        flushing = false
    }

    /** The wait's deadline passed with no answer. True means the newest held screen should be drawn. */
    fun timedOut(): Boolean {
        if (!awaiting) return false
        awaiting = false
        flushing = false
        return true
    }
}
