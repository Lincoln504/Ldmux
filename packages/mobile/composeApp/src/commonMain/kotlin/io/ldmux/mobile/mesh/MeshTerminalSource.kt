package io.ldmux.mobile.mesh

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import io.ldmux.mobile.AltScroll
import io.ldmux.mobile.ClipboardRequest
import io.ldmux.mobile.TapAction
import io.ldmux.mobile.TerminalHandle
import io.ldmux.mobile.TerminalSource
import io.ldmux.mobile.planAltScroll
import io.ldmux.mobile.planTap
import io.ldmux.mobile.Modifiers
import io.ldmux.mobile.SCROLL_OVERLAP
import io.ldmux.mobile.term.TerminalEngine
import io.ldmux.mobile.term.TerminalFrame
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import io.ldmux.mobile.appLog

/**
 * What the shared terminal surface draws on the mesh route.
 *
 * The surface is the tailnet route's own (`TerminalSurfaceFor`): same painter, same gestures,
 * same selection handles, same cursor, same IME field. This is the other end of it. Where the
 * tailnet source is fed by an attach stream, this one is fed by [show], once per screen the host
 * committed, through [meshScreenBytes]; and where the tailnet source sends keystrokes straight
 * out, this one hands them to [input], the batcher that spends the link's few records well.
 *
 * Everything about *input* is [input]'s (`TerminalHandle by input`), so a key, a paste and a
 * sticky modifier behave exactly as they did before this source existed. What is added here is
 * only what the surface asks of a terminal: a frame, a selection, a tap.
 *
 * Not carried, because the link does not carry it: mouse tracking (a tap is always cursor keys).
 * Scrollback *is* offered, but it is the client's own: the host numbers the row at the top of
 * each screen, so [MeshHistory] keeps the rows that left it, and [scrollBy] reads them. Resize
 * *is* carried (profile version 3): the grid the surface measures goes to [onGrid], and the
 * driver asks the host to fit its pty to it, so the host's grid is the phone's and a line is
 * never clipped.
 */
/** How many rows of local history a mesh view keeps. A screenful is what a scroll aims at; this
 * is a few dozen screens, far past what anyone reads back through on a radio. */
private const val MESH_HISTORY_MAX = 2_000

/** How quiet the shell has to be for a synced answer to count as finished. */
internal const val SYNC_SETTLE_MS = 900L

/** The longest a sync can hold the view open, however talkative the program is. */
internal const val SYNC_DRAW_MAX_MS = 8_000L

class MeshTerminalSource(
    private val input: MeshTerminalHandle,
    private val newEngine: (Int, Int) -> TerminalEngine = { c, r -> TerminalEngine(c, r) },
    /**
     * The same monotonic clock the handle stamps input with, for the draw-through window.
     *
     * Required, not defaulted: a fixed clock would leave the window open for ever, which turns
     * batch mode into live mode by accident.
     */
    private val now: () -> Long,
    /**
     * Repeaters on the route to the host, asked afresh each time a wait is sized.
     *
     * **A wait for something that crosses the mesh is a function of how far it crosses.** The
     * history page is one record each way, so on a twenty-hop route the answer takes a minute and a
     * flat ten-second wait declared it lost — the up key then asked again while the first page was
     * still travelling. Defaulted because a source with no link (a test) has no route to ask about.
     */
    private val hops: () -> Int? = { null },
) : TerminalSource, TerminalHandle by input {

    /**
     * While a synced answer is arriving, the view follows it.
     *
     * The wait for an answer ends on the first screen that arrives, which is normally the host
     * echoing what was sent. The *answer* follows in more screens, and those were frozen — so a
     * synced command printed its result into a frozen view and the user had to press `show` to see
     * the outcome of a command they had already sent. Measured on the device, 2026-11-06: `pwd`,
     * enter, sync, and the bar read "view 2 behind" over a screen with no output.
     *
     * **The window is idle-based, not a fixed wait.** Every screen that arrives while it is open
     * pushes the deadline out by [SYNC_SETTLE_MS], so the view follows the answer until the shell
     * stops talking and then freezes again by itself — no fixed delay is ever waited for, and a
     * slow answer is not cut off. [SYNC_DRAW_MAX_MS] is the ceiling, so a program that talks for
     * ever (a build log) cannot hold the view open against the mode's purpose.
     */
    private var drawThroughUntil: Long = 0
    private var drawThroughCeiling: Long = 0

    private var engine: TerminalEngine? = null
    private var cols = 0
    private var rows = 0
    private var last: MeshScreen? = null

    /**
     * The shell the drawn screen belongs to.
     *
     * **This, not a UI effect, is what makes a newly opened shell paint.** The hold draws a screen
     * only when nothing has been painted yet (`acceptScreen(painted = last != null)`), which was
     * judged over the source's whole life: a previous shell's last screen left `last` set, the new
     * shell's first screen was therefore "not the first", and the hold kept it. The screen effect
     * that cleared the source was a separate `LaunchedEffect` keyed on the attached id, so whether
     * it ran before the new shell's first commit depended on effect ordering — and on a public
     * channel, where the first full screen takes seconds, the bar sat at "view 2 behind" over a
     * blank page until a refresh.
     *
     * A screen from a different shell than the last one drawn is the first screen of that shell,
     * by definition, and is treated as one.
     */
    private var drawnShell: String? = null

    /**
     * Rows that have left the live screen, oldest first.
     *
     * The link carries one screen and no history (`MESH-GATEWAY`), so this is the client's own:
     * the host numbers the row at the top of each screen (`firstLine`), and the rows between two
     * of those numbers are exactly what scrolled off. Nothing is invented - an unknown line
     * number accumulates nothing - and the buffer is bounded, because a build log is not a
     * document anyone scrolls to the top of.
     */
    private val history = MeshHistory(MESH_HISTORY_MAX)

    private val _frame = MutableStateFlow<TerminalFrame.Frame?>(null)
    override val frame: StateFlow<TerminalFrame.Frame?> = _frame

    /**
     * Public-channel mode: input waits on the phone and the view does not follow the host.
     *
     * The state is [PublicChannelHold]'s, because the rule is "may this screen be drawn now", and
     * that question has no engine in it. This source is the only thing that consults it, and the
     * screen is the only thing that turns it on or asks it to sync. See ADR-0026.
     */
    private val hold = PublicChannelHold()

    /** The newest screen the host sent while the view was frozen. */
    private var pendingScreen: MeshScreen? = null

    /** A held echo is currently drawn on top of the host's screen. */
    private var echoing: Boolean = false

    /**
     * The held text as it should be echoed, kept so a repaint can put it back.
     *
     * Empty when nothing is held. Without this the echo lasted only until the next paint: press
     * sync with a command still queued behind one in flight, and the answer's repaint erased the
     * text while the bar went on counting it — text the screen no longer showed and the user
     * could not see to correct. Now every paint writes "the host's screen, plus what is still
     * held", which is the same invariant [echoHeld] states, just also true after a repaint.
     */
    private var echoText: String = ""

    /**
     * The screen [MeshHistory] was last fed as "the new one", so a run of screens held while the
     * view was frozen is noted once each instead of being skipped (frozen screens were never
     * noted) or double-noted (the pull notes the same pair [show] just did).
     */
    private var notedScreen: MeshScreen? = null

    /** The mode is engaged: typing is held and the view is frozen until a sync. */
    val publicChannel: Boolean get() = hold.on

    /** A sync is out and its answer has not arrived: the view follows incoming screens. */
    val awaitingSync: Boolean get() = hold.awaiting

    /**
     * A press that actually sent something is waiting for its answer.
     *
     * What the bar may claim. A `show` press with an empty buffer unfreezes the view but owes no
     * answer, so "sent · …" about it would be a control describing a send that never
     * happened.
     */
    val awaitingAnswer: Boolean get() = hold.awaiting && hold.waitingOnSend

    /**
     * Whether a screen has been drawn at all.
     *
     * Before the first paint there is nothing to freeze, and the first paint has to arrive: the host
     * is not asked to hold until this is true.
     */
    val hasScreen: Boolean get() = last != null

    /**
     * A screen the hold is keeping back, waiting for something to release it.
     *
     * The bar draws it as "output waiting · show": without that line the two states a `show` press
     * can be in (nothing new was sent, or a newer screen is held) read the same, so the one press
     * that would reveal a command's output had nothing advertising it.
     */
    val heldScreenWaiting: Boolean get() = pendingScreen != null

    /** When the last sync was pressed, on the same clock this class reads. */
    val pressedAt: Long get() = hold.pressedAt

    /** The deadline passed and the bar should say so. Cleared by the answer or by the next press. */
    val answerOverdue: Boolean get() = hold.reported

    /**
     * The buffer is being drained: the pump may send right now.
     *
     * True only from the sync press until the buffer is empty. [awaitingSync] stays true after
     * that, while the answer is on its way — and typing during that wait is held, because "no
     * data is sent live" does not pause just because a send is in flight.
     */
    val flushingHeld: Boolean get() = hold.flushing

    /** Screens the host sent while the view was frozen; the newest is what a pull shows. */

    /**
     * How many syncs have been pressed. The screen keys its answer deadline on this rather than on
     * [awaitingSync]: a Boolean that stays true cannot restart a deadline when the user presses
     * sync again, so a second press 15 s into a 20 s wait would inherit 5 s of clock it did not earn.
     */
    val syncSeq: Int get() = hold.seq

    /**
     * Turn the mode on or off. Turning it off draws whatever the host sent meanwhile, so leaving
     * the mode never leaves a stale screen claiming to be live.
     */
    fun setPublicChannel(on: Boolean) {
        if (on == hold.on) return
        hold.set(on)
        if (!on) pullHeld()
    }

    /**
     * The sync button's action. True when something is waiting and the pump must flush it; false
     * when nothing was waiting, in which case the newest held screen is drawn now.
     *
     * The return value is informational: the pump polls its gate every 120 ms while the mode is
     * on, so nothing has to be woken. The bar reads the same facts it does (`flushing`, `awaiting`)
     * and the deadline is [syncSeq].
     *
     * It cannot be a synchronous request and answer: sending queues records on the link and the
     * host's answer arrives as its next `Commit`, so the answer to "did it work" is "the next
     * screen". The screen's own deadline is what stops a wait nobody will answer.
     */
    fun sync(): Boolean {
        val pending = input.hasPending
        // Nothing typed and no screen held: the view is already the newest the host sent, so there
        // is nothing to send and nothing to show. Starting a wait here would be a wait for an
        // answer that cannot come, and the deadline would report a failure that did not happen.
        if (!pending && pendingScreen == null) {
            // **"Show me the shell now", even when nothing is held.** The host is not drawing a
            // frozen view (see the hold effect), so a `show` with nothing typed has to *release*
            // it or the button does nothing at all: measured 2026-11-09, `show` on a batch shell
            // never refreshed the screen, because with the hold there is never a pending screen to
            // draw. Releasing lets the host send the difference, which is usually nothing — and
            // that is the correct answer when the screen is already current.
            if (hold.on && last != null) {
                // **"Show me the shell" is one record out and the difference back.** The host keeps
                // holding and draws once, so this is `Resync` alone — no release, no re-hold, and no
                // state on this side that can disagree with the host's. It used to be three records
                // a press: `Hold{off}`, the frame, `Hold{on}`.
                hold.sync(pending = false, nowMs = now())
                onWantScreen?.invoke()
                return false
            }
            // Nothing to send, nothing held and nothing to release: a wait left over from an
            // earlier press must end here, or the bar would describe a wait the button cannot act
            // on.
            hold.settle()
            return false
        }
        // **A press is not typing.** The batcher's windows coalesce keystrokes; the press that
        // ends the composition should leave now, not one window later.
        if (pending) input.flushNow()
        if (hold.sync(pending, now())) {
            drawThroughUntil = now() + SYNC_SETTLE_MS
            drawThroughCeiling = now() + SYNC_DRAW_MAX_MS
            return true
        }
        // "Show": draw the newest held screen and end the wait — the user asked to see the shell,
        // not to wait for an answer to a send that did not happen.
        pullHeld()
        return false
    }

    /**
     * Drop everything held and every rendering of it (the screen going away).
     *
     * Held text is the one plaintext this app keeps, and in manual-sync mode it can sit for
     * minutes. Persistence was never the risk (nothing is saved); surviving in the background was.
     */
    fun scrub() = input.scrub()

    /**
     * The pump found nothing left to send.
     *
     * If a sync was in flight and the host's answer already arrived during the drain, it was held
     * (see [PublicChannelHold.acceptScreen]) — and this is where "send what is held, then show the
     * shell's view" is actually honoured: the send finished, so the answer is now drawn. Without
     * it the answer sat in [pendingScreen] until the user pressed the button again.
     */
    fun syncDrained() {
        val wasFlushing = hold.drained()
        // **The window opens when the send completes, not when the button was pressed.** Draining a
        // multi-record command takes seconds on this link, and during the drain screens are held on
        // purpose (they are the pty's reaction to a partial command). Arming the window at the press
        // spent its life on the drain and closed it just as the answer began to arrive.
        if (wasFlushing) drawThroughUntil = now() + SYNC_SETTLE_MS
        if (!hold.awaiting) return
        // **The answer that arrived during the drain is the answer.** It used to be drawn with the
        // wait deliberately kept (`drawHeldKeepingWait`), on the reasoning that the wait ends with
        // the *next* screen — which is true on a shell that keeps printing and false on every shell
        // that then sits still. On an idle shell no further screen ever came, so the bar said
        // "waiting for the answer" over a screen the user was already looking at, and the only way
        // out was pressing `show`. The send is complete and its answer is here: end the wait.
        // The timeout path keeps its old behaviour, because there the answer genuinely has not
        // arrived.
        if (!pullHeld()) drawHeldKeepingWait()
    }

    /**
     * The wait's deadline passed with no answer: draw what is held and say so.
     *
     * **The wait survives it.** Drawing what is held does not end the wait, because the answer to a
     * sync is "the next screen the host sends" and a slow answer is the normal answer on this link.
     * Clearing the wait here meant the late answer was *held* and the user pressed the button again
     * — the press they had already made, spent on nothing. The bar says "no answer · …" until the
     * answer arrives, and the answer is drawn when it does.
     */
    fun syncTimedOut(): Boolean {
        if (!hold.timedOut()) return false
        drawHeldKeepingWait()
        return true
    }

    /**
     * Draw the newest held screen without ending the wait.
     *
     * The difference from [pullHeld] is one flag: [PublicChannelHold.painted] ends the wait, and a
     * paint that happens *while* waiting (a drain finishing, a deadline passing) must not, or the
     * answer that follows is held instead of drawn.
     */
    private fun drawHeldKeepingWait(): Boolean {
        val held = pendingScreen ?: return false
        pendingScreen = null
        applyScreen(held)
        return true
    }

    /**
     * Why a scroll did nothing, while it is worth saying.
     *
     * The mesh pad's arrows are the same three controls as the tailnet's, and here they can do
     * nothing at all: the host sends no history and the client accumulates its own, so a session
     * that has not scrolled yet has no history to scroll into. A button that returns 0 with no
     * word about it reads as broken — measured on the device 2026-11-03. Cleared by the next
     * scroll that moved, or by the screen after it has shown it.
     */
    var scrollNotice: String? by mutableStateOf(null)
        private set

    /** The host's `Key` names are the tailnet's, but the mesh bar sends no `S-` forms. */
    override val shiftKeys: Boolean get() = false

    /**
     * Never called: nothing on this link can set the clipboard (the rows are plain text with every
     * control character removed, so an OSC 52 cannot reach the engine). Kept because the surface
     * assigns it.
     */
    override var onClipboardRequest: ((ClipboardRequest) -> Unit)? = null

    override fun setActive(value: Boolean) {}

    /** Where the surface's measured grid goes: the driver's request to the host to fit its pty. */
    var onGrid: ((Int, Int) -> Unit)? = null

    /**
     * The grid the surface can paint at the user's own font size, measured by its painter. The
     * tailnet sends the same number to the daemon as a resize; here it is the `Resize` message.
     */
    override fun setGrid(cols: Int, rows: Int) {
        onGrid?.invoke(cols, rows)
    }

    /**
     * Put [screen] on the engine. Called for every change of the committed screen, on the main
     * thread (the engine is single-threaded). The same screen twice is skipped, because a repaint
     * costs a frame decode and nothing changed.
     *
     * A grid that changes size (the host re-described its terminal) is a new engine: the old one's
     * cells are the wrong shape, and a selection into them would mean nothing.
     */
    fun show(screen: MeshScreen, shell: String? = null) {
        if (screen.cols <= 0 || screen.lines.isEmpty()) return
        // One shell's rows are never drawn under another's name, and a new shell starts unpainted.
        if (shell != null && shell != drawnShell) {
            forgetScreen()
            drawnShell = shell
        }
        // The hold decides whether this screen may be drawn. While frozen the newest screen is
        // kept rather than drawn, so a sync shows the shell as it is now and not a replay of what
        // was typed. Nothing else about the link changes: the driver keeps polling and the
        // session keeps its acks, because a frozen view is a view and not a dead link.
        // **"Painted" means painted into the engine in front of the user, not "at some point".**
        // A grid change (the host fitting the pty to the keyboard) builds a new engine and blanks
        // the surface, so the screen that follows has nothing behind it — yet `last` was still set
        // from the previous engine, the hold called it "not the first screen", and the page stayed
        // empty until a sync. Measured 2026-11-06: the first screen drew at 80x24, the fit
        // re-described the terminal as 46x19, and every screen after that was held.
        val rebuilt = engine == null || screen.cols != cols || screen.lines.size != rows
        // A sync opens the view until the answer stops arriving: the answer is not one screen.
        val t = now()
        val open = t < drawThroughUntil && t < drawThroughCeiling
        if (open) drawThroughUntil = minOf(t + SYNC_SETTLE_MS, drawThroughCeiling)
        // **A drain is not overridden by the window.** Mid-send, a screen is the pty's reaction to a
        // partial command; drawing it would show half a command's output as if it were the answer,
        // which is the invariant `acceptScreen` states and the window must not quietly break.
        val accepting = open && !hold.flushing
        // **While a sync's answer is arriving, the view follows it.** `awaiting` stays true until the
        // shell has been quiet for a round trip ([settleIfQuiet]), so the echo, the command's output
        // and the prompt after it all draw — instead of only the first, with the rest held behind a
        // `show` press. A drain still overrides this: mid-send a screen is a partial command's
        // reaction, not the answer.
        val answering = hold.awaiting && !hold.flushing
        val accept = answering || accepting || hold.acceptScreen(painted = last != null && !rebuilt)
        // Which shell a screen belongs to, and whether the hold took it, is the one fact that
        // decides whether a freshly opened shell paints. Logged at the decision, because the two
        // plausible causes (a stale `painted`, a shell id that never changed) look identical from
        // the outside — both produce a blank page and a bar counting held screens.
        appLog("mesh show shell=$shell drawn=$drawnShell painted=${last != null} held=${!accept}")
        if (!accept) {
            // **A screen held is a screen not noted, and that loses scrollback.** The rows that
            // left while the view was frozen are the same rows the host numbered, so they are
            // history whether or not the screen was drawn: a build log watched through a freeze
            // must scroll back to its beginning afterwards. [notedScreen] chains the pairs so each
            // one is noted exactly once, and the pull that draws the newest notes its own pair.
            history.note(notedScreen ?: last, screen)
            notedScreen = screen
            pendingScreen = screen
            return
        }
        pendingScreen = null
        applyScreen(screen)
        hold.drawn(now())
        hold.painted()
    }

    /**
     * Forget the screens the hold kept back, without drawing them.
     *
     * The held screen is the answer to something that happened on **this page**: a sync waiting
     * for its answer, or a shell's output piling up while the view was frozen. Leaving the page
     * ends the question, so returning to it must not resume a count of "view N behind" from a
     * visit that is over — the same rule as the typing that went with it.
     */
    /**
     * End a sync's wait when the shell has gone quiet, and report whether it did.
     *
     * Called on a short tick by the screen while a wait is running. This is what turns "waiting for
     * the answer" off on an idle shell that has finished answering — without it the bar kept saying
     * the wait was on, or the view froze mid-answer and needed `show`.
     */
    fun settleIfQuiet(): Boolean = hold.settleIfQuiet(now())

    /** The host said the pane has stopped changing: see [PublicChannelHold.answered]. */
    fun answered(): Boolean = hold.answered()

    fun discardHeldScreens() {
        pendingScreen = null
    }

    /**
     * Draw the newest screen the host sent, without sending anything.
     *
     * The honest answer to "sync" when nothing is waiting: there is nothing to send, so the only
     * thing a button can do is show what the shell last said. False when no screen is held.
     */
    fun pullHeld(): Boolean {
        val held = pendingScreen ?: return false
        pendingScreen = null
        applyScreen(held)
        hold.painted()
        // **A pulled screen is a drawn screen**, and a drawn screen is the whole answer to a
        // `show` that sent nothing: the wait ends here, so the host is re-held instead of
        // streaming into a frozen view.
        hold.drawn(now())
        return true
    }

    private fun applyScreen(screen: MeshScreen) {
        val c = screen.cols
        val r = screen.lines.size
        if (c <= 0 || r <= 0) return
        io.ldmux.mobile.appLog("mesh show ${c}x$r engine ${cols}x$rows offset ${history.offset} same ${screen == last}")
        if (screen == last && engine != null) {
            // A screen identical to the one drawn may still need drawing: if a held echo is on
            // top of it, this is the moment the echo goes and the truth comes back. Without this
            // a sync answered by an unchanged screen left the fake text on screen for good.
            if (echoing) {
                echoing = false
                paint()
            }
            return
        }
        // Keep the rows that left before the new screen replaces the old one. A reflow (a new
        // grid) or a jump backwards is not a scroll, and adds nothing. The previous screen is the
        // last one history saw, which after a freeze is the last *held* screen, not the drawn one.
        val previous = last
        if (screen != notedScreen) history.note(notedScreen ?: previous, screen)
        notedScreen = screen
        if (engine == null || c != cols || r != rows) {
            val reflow = engine != null && c != cols
            engine?.close()
            engine = newEngine(c, r)
            cols = c
            rows = r
            // **A width change is a reflow; a height change is not a reason to leave history.** The
            // view used to go to the live bottom whenever either changed, so a shell whose last row
            // appeared and disappeared (a prompt, a status line) yanked the operator out of the
            // history they had scrolled to — measured on the device 2026-11-09 as `offset 33` then
            // `offset 0` two screens later. The offset counts rows above the live bottom, and
            // `window()` clamps it, so a shorter grid keeps the position it can.
            if (reflow) history.toBottom()
            hasSelection = false
        }
        last = screen
        // A view parked in history is somewhere the user put it. New output must not drag it
        // back to live; the pad's bottom button is how it returns.
        if (history.offset == 0) paint()
    }

    /**
     * Write what is being held into the engine, so the user sees their typing at the cursor.
     *
     * This is a **fake**, and it is built so that it cannot drift: every call rebuilds the host's
     * last screen first and then writes the held text on top, so the echo is always exactly "the
     * shell as it was, plus what you have typed since". Painting each keystroke onto the previous
     * echo would accumulate an image that disagrees with the screen it is a fake of, and no amount
     * of care about backspace and wrapping keeps that true.
     *
     * Two deliberate limits:
     *
     *  - **Autowrap is off while the echo is written, and the draft is cut to the cells that are
     *    left on this row.** A wrapped line would scroll the screen, and a row pushed into the local
     *    scrollback by a fake is history that never happened; a draft written past the last column
     *    overwrote that column on top of itself. [draftEcho] windows it instead.
     *  - **Nothing is echoed while the view is parked in history** ([MeshHistory.offset]). The
     *    user is reading rows above the live screen; typing into the middle of them would be a
     *    fake of the wrong screen.
     *
     * An empty [text] removes the echo and draws the host's screen again.
     */
    fun echoHeld(text: String) {
        // Remembered, not just drawn: [paint] puts it back on the host's next screen, so a repaint
        // while text is still held does not erase the text the bar is still counting. Remembered
        // **as it is**, because turning it into cells needs the row and the width it will be drawn
        // at, and those are known at paint time and not before ([draftEcho]). Nothing here is a
        // safety filter: that lives in [draftEcho] too, which is the one thing that writes the
        // draft into an engine that executes escape sequences.
        echoText = text
        paint()
    }

    /** Draw the live screen, or the history window at [offset] rows above it. */
    private fun paint() {
        echoing = false
        val screen = last ?: return
        val e = engine ?: return
        if (history.offset == 0) {
            val bytes = meshScreenBytes(screen, cols, rows)
            e.write(bytes, bytes.size)
            // **The held text goes back on top.** A paint is "the host's screen as it is now", and
            // the held text has not been sent yet, so the honest picture is both. Not on a
            // full-screen program, though: the same rule VS Code's local echo follows for `vim`,
            // `less` and `tmux` — a program that repaints the screen decides where text lands, so
            // a fake written at the cursor lands in somebody else's drawing. It stays counted in
            // the bar instead. Nothing is echoed in history either, where the rows above the live
            // screen are not where typing goes.
            if (echoText.isNotEmpty() && !screen.alternate) {
                // Autowrap off around the whole echo, and faint (`ESC[2m`) around the text:
                // provisional text looks provisional. That is the one thing a fake must never be —
                // mistakable for the shell's own output, which is what the same characters will
                // look like when the send comes back.
                //
                // **[draftEcho] decides the cells, and it can decide them as empty.** A draft with
                // nothing visible in it (a lone arrow key) draws nothing at all, and writing the
                // escape pair around an empty string would leave the engine in the faint pen for
                // nothing.
                val drawn = draftEcho(echoText, screen.cursorX, cols)
                if (drawn.isNotEmpty()) {
                    val payload = "\u001b[?7l\u001b[2m" + drawn + "\u001b[22m\u001b[?7h"
                    val held = payload.encodeToByteArray()
                    e.write(held, held.size)
                    echoing = true
                }
            }
        } else {
            val window = screen.copy(
                lines = history.window(screen, rows),
                cursorX = 0,
                cursorY = rows - 1,
                dead = false,
            )
            val bytes = meshScreenBytes(window, cols, rows, showCursor = false)
            e.write(bytes, bytes.size)
        }
        _frame.value = e.render()
    }

    override fun repaint() {
        _frame.value = engine?.render()
    }


    /**
     * Forget the screen: the engine and its frame go, so the surface draws nothing until the next
     * [show]. Used when the host is attached to another shell, so that one shell's rows are never
     * drawn under another's name.
     */
    fun clear() {
        forgetScreen()
        drawnShell = null
    }

    /** Drop the engine, the drawn screen and everything that belonged to it. */
    private fun forgetScreen() {
        // The bar counts held screens; a page that holds none must not say it holds two.
        hold.forget()
        engine?.close()
        engine = null
        last = null
        cols = 0
        rows = 0
        history.clear()
        hasSelection = false
        _frame.value = null
        // The echo belongs to a screen that no longer exists, and so does the row history was last
        // fed: both would be a fake of the previous shell drawn under the next one's name.
        echoText = ""
        echoing = false
        notedScreen = null
        // A screen from the shell that is gone is not an answer to anything: drop it so a sync
        // does not draw the last shell's rows under the next shell's name.
        pendingScreen = null
        scrollNotice = null
    }

    /** Free the engine. Idempotent. */
    fun close() {
        engine?.close()
        engine = null
    }

    // ── selection: the engine's, exactly as the tailnet route's ─────────────────────────────

    override var hasSelection by mutableStateOf(false)
        private set

    override fun selectionGeometry(columns: Int, cellW: Int, cellH: Int, padLeft: Int, screenH: Int) {
        engine?.setGeometry(columns, cellW, cellH, padLeft, screenH)
    }

    override fun selectionSet(startCol: Int, startRow: Int, endCol: Int, endRow: Int): Boolean {
        val changed = engine?.selectionSet(startCol, startRow, endCol, endRow) ?: false
        refreshSelection()
        return changed
    }

    override fun selectionBounds(): IntArray? = engine?.selectionBounds()

    override fun selectionText(): String? = engine?.selectionText()

    override fun clearSelection() {
        if (engine?.selectionClear() == true) refreshSelection()
    }

    override fun selectAll() {
        engine?.selectionSelectAll()
        refreshSelection()
    }

    private fun refreshSelection() {
        hasSelection = engine?.selectionText() != null
        _frame.value = engine?.render()
    }

    /**
     * Scroll the client's own history, or - on the alternate screen, which has none - send the
     * program the keys a wheel would. Negative is up, as everywhere else.
     */
    /**
     * Typing goes to the prompt, and the prompt is the live bottom.
     *
     * The same rule the tailnet route follows, stated here because the two handles are
     * separate: reading history and then typing sent the keys to a prompt off screen, and
     * nothing moved, which is indistinguishable from a link that stopped listening.
     */
    private fun followInput() {
        if (history.offset != 0) scrollToBottom()
    }

    override fun sendKey(name: String) {
        followInput()
        input.sendKey(name)
    }

    override fun sendModifiedKey(key: String, modifiers: Modifiers) {
        followInput()
        input.sendModifiedKey(key, modifiers)
    }

    override fun sendText(text: String) {
        if (text.isEmpty()) return
        followInput()
        input.sendText(text)
    }

    override fun paste(text: String): Boolean {
        if (text.isEmpty()) return true
        followInput()
        return input.paste(text)
    }

    override fun scrollPage(direction: Int) {
        if (rows <= 0) return
        val step = io.ldmux.mobile.pageStep(rows)
        scrollBy(if (direction < 0) -step else step)
    }

    override fun scrollBy(rows: Int, pageInHistory: Boolean): Int {
        val screen = last ?: return 0
        if (screen.alternate) {
            val f = _frame.value ?: return 0
            return when (val action = planAltScroll(rows, f, mouseRefused = true)) {
                is AltScroll.Keys -> {
                    for (name in action.keys) input.sendPlainKey(name)
                    0
                }
                is AltScroll.Wheel -> 0
                AltScroll.None -> 0
            }
        }
        val moved = history.scroll(rows)
        if (moved != 0) {
            scrollNotice = null
            awaitingHistory = false
            paint()
        } else if (rows < 0) {
            askForHistory(rows)
        } else {
            scrollNotice = null
        }
        return moved
    }

    /**
     * The up key ran out of locally accumulated history, so **ask the host for the page above**.
     *
     * **The keeper has the rows; the link only carries the live screen.** A phone that joined a
     * shell late, or was frozen while the shell printed, cannot have them from its own
     * accumulation — and answering "no history yet on this link" was true of this client and
     * useless to the operator, whose up key means "fetch a page". The request names the absolute
     * line immediately above the oldest row held here, so a page joins what is held instead of
     * overlapping it.
     *
     * A host that cannot number its lines (`firstLine == 0`) has no line to ask for, and a keeper
     * with no history above has nothing to give: the first keeps the old sentence, the second gets
     * "start of history" from the empty page.
     */
    private fun askForHistory(rows: Int) {
        val screen = last
        val top = screen?.let { if (it.firstLine > 0) it.firstLine.toInt() - history.size else 0 } ?: 0
        val want = (screen?.lines?.size ?: 17).coerceIn(1, MeshTerm.MAX_HISTORY_ROWS)
        val ask = onWantHistory
        if (top <= 0 || ask == null) {
            scrollNotice = if (history.size == 0) "no history yet on this link" else "start of history"
            paint()
            return
        }
        // **`rows` counts what the press wants, `want` is a page.** The host serves at most one
        // page-sized answer per press, so a press never costs two records.
        // **A lost request expires; it does not block the key for ever.** The radio's own
        // retransmission is bounded (six airtimes per hop), so a page that has not arrived by
        // [HISTORY_WAIT_MS] is treated as lost and the next press asks again.
        if (awaitingHistory && now() - historyAsked < answerDeadlineMs(HISTORY_WAIT_MS, hops())) return
        awaitingHistory = true
        historyAsked = now()
        scrollNotice = "fetching history"
        // **Fetch a page, move by the press.** A page is one request either way, so a one-row key
        // asks for the same page a page key does and then lands one row above where it was: the
        // rest of the page is already in hand for the next press, and no press costs two records.
        pendingAdvance = (-rows).coerceIn(1, want)
        ask((top - want).coerceAtLeast(0), want)
        paint()
    }

    /** Wired by the screen to the driver: ask the host for [rows] lines from absolute line [from]. */
    var onWantHistory: ((Int, Int) -> Unit)? = null

    /**
     * Ask the host to draw the shell once, without releasing the freeze.
     *
     * Set by the screen to [`MeshTermDriver.showOnce`]. Batch mode's `show` press is this request and
     * nothing else: the host answers with the difference from the screen this client holds and goes
     * on holding, so a look costs two records instead of a release, a frame and a re-hold.
     */
    var onWantScreen: (() -> Unit)? = null

    /** A history request is in flight, so an up key pressed again does not queue another. */
    private var awaitingHistory = false

    /** When it went out, so a lost request does not leave the up key dead for ever. */
    private var historyAsked = 0L

    /** How many rows the press that asked for the page wanted to move. */
    private var pendingAdvance = 1

    /**
     * A page of the shell's own scrollback arrived: prepend it and draw it.
     *
     * An empty page is the host's answer that nothing is above — `last` alone cannot say that, since
     * a full page and an empty one are both "last" of their answer.
     */
    private companion object {
        /**
         * How long a history request may be in flight before the up key may ask again, before the
         * route's own answer deadline is added to it.
         */
        const val HISTORY_WAIT_MS = 10_000L
    }

    /**
     * **The answer to an up key: rows from the shell's own scrollback, placed where they belong.**
     *
     * The page states the absolute line of its first row, so it is placed by what it says rather
     * than by what was asked for. Three cases, and all three are real:
     * - it ends exactly at this client's oldest row (the normal case): prepend it;
     * - it overlaps rows already held (the host served from a line this client has): trim the tail,
     *   so nothing is shown twice;
     * - it ends above this client's oldest row (the pane dropped those rows since the request): the
     *   gap is named in the notice rather than spliced over, because inventing rows is worse than
     *   showing a break.
     *
     * `older` says whether the pane holds more history above, so the operator learns "start of
     * history" without spending a record to find out.
     */
    fun prependHistory(page: TermMessage.HistoryPage) {
        awaitingHistory = false
        // The press that asked owns its movement, once: a later page must not inherit it.
        val advance = pendingAdvance
        pendingAdvance = 1
        val screen = last ?: return
        if (page.lines.isEmpty()) {
            // An empty answer is a fact: the pane had nothing to serve, and `older` says whether
            // that is the top of the history or a request the host could only clamp.
            scrollNotice = if (page.older) "history is above this" else "start of history"
            paint()
            return
        }
        // **Where the page belongs, in the host's own numbering.** `top` is the absolute line of
        // this client's oldest held row, so rows at or above it are new and rows below it are ones
        // already on screen. Placing by number is what makes an overlap harmless and a gap visible:
        // the alternative, assuming every page joins, shows rows twice or splices a hole shut.
        val top = if (screen.firstLine > 0) screen.firstLine.toInt() - history.size else 0
        val pageEnd = page.from + page.lines.size
        val take = (minOf(pageEnd, top) - page.from).coerceAtLeast(0)
        val missing = (top - pageEnd).coerceAtLeast(0)
        if (take == 0) {
            // Every row of the page is one this client already holds, or below it.
            scrollNotice = if (page.older) null else "start of history"
            paint()
            return
        }
        val keep = page.lines.subList(0, take)
        scrollNotice = when {
            missing > 0 -> "history has a gap here"
            page.older -> null
            else -> "start of history"
        }
        // `offset` is read **before** the prepend: `MeshHistory.prepend` parks itself on the new
        // top, so asking afterwards would measure the wrong position.
        val held = history.size
        val was = history.offset
        val atTop = was >= held
        history.prepend(keep)
        history.park(if (atTop) held + advance else was + keep.size)
        paint()
    }

    override fun scrollToBottom() {
        scrollNotice = null
        if (history.offset == 0) return
        history.toBottom()
        paint()
    }

    /**
     * Rows this view is parked above the live bottom; 0 means it is following the shell.
     *
     * Read-only, and the observable the key bars are judged by: a bar key goes through [sendKey],
     * which follows the input back to live, while the page's own batcher would send it into a
     * prompt the user cannot see. That is RELEASE-AUDIT 9.14, and it was fixed on the tailnet and
     * reopened here, so it is a fact worth being able to assert.
     */
    internal val scrolledRows: Int get() = history.offset

    /** The screen has shown [scrollNotice]; stop drawing it. */
    fun clearScrollNotice() { scrollNotice = null }

    /**
     * A tap moves the shell's cursor with the arrow keys, by the same plan as the tailnet route
     * ([planTap]) — the mouse path is never taken, since nothing here reports mouse tracking. The
     * keys go through [input], so a long walk is coalesced into a few records and a refusal (the
     * queue is full) is reported as a tap that did nothing, with no outline drawn.
     */
    override fun tapCell(col: Int, row: Int): Boolean {
        val f = _frame.value ?: return false
        return tapOn(f, col, row)
    }

    /** [tapCell] against a given frame, so the rule is testable without a native engine. */
    internal fun tapOn(f: TerminalFrame.Frame, col: Int, row: Int): Boolean {
        // **A frozen view has no cursor to walk from.** The picture is the one the operator last
        // asked for, so the cursor drawn on it is not where the shell's cursor is now: walking from
        // it moves the real cursor to a place the operator misread. Batch mode also cannot show the
        // result of the walk until a `show`, so the keys pile up invisibly and land all at once —
        // measured 2026-11-09 as a screenful of `^[[D` in a shell whose loop reads no input, after a
        // few taps were flushed by a `show` press.
        if (hold.on && !hold.awaiting) return false
        return when (val action = planTap(col, row, f, mouseRefused = true)) {
            is TapAction.Keys -> {
                var all = true
                for (name in action.keys) all = tapKey(name) && all
                all
            }
            else -> false
        }
    }

    /** A tap's arrow key never spends a Ctrl the user armed for their next keystroke. */
    private fun tapKey(name: String): Boolean = input.sendPlainKey(name)
}
