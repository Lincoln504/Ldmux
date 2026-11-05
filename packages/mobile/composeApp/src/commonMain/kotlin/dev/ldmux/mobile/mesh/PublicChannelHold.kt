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

    /**
     * How many syncs have been pressed. A monotonic count, not a flag, because the screen's
     * deadline is keyed on it: a second press while a wait is running must restart the clock, and
     * a Boolean that stays true cannot tell a re-launched effect that anything changed.
     */
    var seq: Int by mutableStateOf(0)
        private set

    /**
     * The deadline passed and the user has been told. It changes what the bar *says*, and nothing
     * else — [awaiting] deliberately survives it.
     *
     * The deadline used to clear the wait, so an answer that arrived ten seconds late was *held*
     * and the user had to press the button a second time to see it: the press they had already
     * made, spent on nothing. An answer is an answer whenever it arrives, and the slow answer is
     * the normal one on a congested channel — the very case this mode exists for.
     */
    var reported: Boolean by mutableStateOf(false)
        private set

    /** Turn the mode on or off. Turning it off clears any wait; the caller draws what it has. */
    fun set(value: Boolean) {
        if (on == value) return
        on = value
        awaiting = false
        flushing = false
        reported = false
        updates = 0
    }

    /**
     * Nothing to send and nothing held to show: stop waiting.
     *
     * "Show" with an empty buffer used to do literally nothing, so a wait left over from an earlier
     * press kept the bar saying "waiting for the answer" — a control reporting a wait for something
     * it cannot receive. [awaiting] stays false here on purpose: the user has just asked to see the
     * shell and there is nothing to show, so the next screen belongs to the frozen view like any
     * other, not to a sync.
     */
    fun settle() {
        awaiting = false
        reported = false
    }

    /** The held screen was drawn without ending the wait: the answer may still be coming. */
    fun drewHeld() {
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
        if (!on) return true
        if (!painted) return true
        // **Mid-send, a screen is held too.** The drain leaves in more than one record on this
        // link (`QueueFull` is the normal case, not the error case), and a screen that arrives
        // between two records is the pty's reaction to a partial command. Drawing it would end
        // the wait, close the drain and strand the rest of what the user typed — measured in
        // review as the reason a two-line command sent only its first line. The answer is only
        // the answer when the send is complete.
        if (flushing) {
            updates += 1
            return false
        }
        if (awaiting) return true
        updates += 1
        return false
    }

    /**
     * A screen was drawn. The wait for the answer is over — [flushing] is *not* touched here.
     *
     * It used to clear both, which is the bug this separation was written for and then broken by:
     * a screen arriving while the buffer was still draining closed the drain, so the pump parked
     * with text still held and nothing in flight to send it. The drain ends on [drained] (the
     * buffer emptied), on [timedOut] (the deadline), or when the mode is turned off — never on a
     * paint. See [acceptScreen] for why a paint mid-drain does not even happen.
     */
    fun painted() {
        awaiting = false
        reported = false
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
        reported = false
        seq += 1
        return pending
    }

    /**
     * The buffer is empty: stop sending. Called by the pump's own loop, not by the answer.
     *
     * True when a drain was actually running, so the caller knows whether it just finished a send
     * or merely looked at an idle batcher. [awaiting] deliberately survives: the send is over and
     * the answer is still owed, which is the pair of facts the bar draws.
     */
    fun drained(): Boolean {
        if (!flushing) return false
        flushing = false
        return true
    }

    /**
     * The wait's deadline passed with no answer. True means the newest held screen should be drawn.
     *
     * It **ends the drain but not the wait**, and the asymmetry is the point:
     *
     *  - The drain stops. Twenty seconds with text still held means the pump has been trying for
     *    longer than this buffer's records take at ~600 ms each, so the link is not carrying them;
     *    continuing costs airtime for a send the user is about to be told may be down. What stays
     *    held stays counted in the bar, so nothing is silently dropped — the next press sends it.
     *  - The wait does not stop. A congested channel answers slowly rather than never, and the very
     *    case this mode exists for is the slow one. Cancelling the wait meant the answer that came
     *    ten seconds late was *held*, and the user pressed the button again — the press they had
     *    already made, spent on nothing. [reported] is what changes: the bar says "no answer yet"
     *    until the answer arrives, and the answer is drawn when it does.
     */
    fun timedOut(): Boolean {
        if (!awaiting) return false
        reported = true
        flushing = false
        return true
    }
}
