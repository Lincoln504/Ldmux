package io.ldmux.mobile.mesh

/**
 * The screen's vocabulary for the mesh terminal, kept apart from any one driver.
 *
 * These values say what the terminal is doing; they do not say how a byte gets to the air. That
 * distinction is why they live here: [MeshTermDriver] (the v14 frame loop) and [MeshEngineDriver]
 * (the v15 engine) both produce them, so the screen and its tests speak one language while the
 * protocol underneath changes. `docs/RADIO-PLAN.md` §7 and `AR-12`.
 *
 * Every state here is a fact the operator can act on. The three "it is not working" values are
 * different values on purpose: the cure for each is different, and a screen that collapsed them
 * into one sentence would tell the user to check a radio that is merely out of range.
 */

/** The features a link may not carry, named for the UI (`Profile.unsupported` bits). */
enum class MeshTermFeature(val bit: Int) {
    Scrollback(MeshTerm.Unsupported.SCROLLBACK),
    Style(MeshTerm.Unsupported.STYLE),
    Hyperlinks(MeshTerm.Unsupported.HYPERLINKS),
    Mouse(MeshTerm.Unsupported.MOUSE),
    Images(MeshTerm.Unsupported.IMAGES),
    Resize(MeshTerm.Unsupported.RESIZE),
    Clipboard(MeshTerm.Unsupported.CLIPBOARD),
}

/** The features the host said are absent, in a fixed order. The UI must not offer these. */
fun absentFeatures(unsupported: Int): List<MeshTermFeature> =
    MeshTermFeature.entries.filter { unsupported and it.bit != 0 }

/** A painted screen. Plain text, one string per row; never half a screen. */
data class MeshScreen(
    val lines: List<String>,
    val cursorX: Int,
    val cursorY: Int,
    /** A full-screen program is running: rewrites are wholesale, so updates are large and slow. */
    val alternate: Boolean,
    /** The shell has exited. */
    val dead: Boolean,
    val epoch: Int,
    /** The host's column count from its `Profile`; the rows are [lines]. 0 until a profile arrived. */
    val cols: Int = 0,
    /**
     * The host's absolute line number for the first of [lines] (0 when it has none). Two screens
     * whose `firstLine` differ by N say that N rows left the top into history, which is the fact a
     * local scrollback needs and a mirror of screen positions cannot invent.
     */
    val firstLine: Int = 0,
)

/**
 * What the mesh terminal is doing, as one value. The three "it is not working" cases are
 * different values on purpose: the cure for each is different.
 */
sealed interface MeshTermView {
    /** Nothing started yet. */
    data object Idle : MeshTermView

    /**
     * The link is not usable, or the session beneath it gave up. [link] says the kind of failure;
     * [detail] is the engine's own sentence about this one, when it has one.
     */
    data class LinkDown(val link: MeshLinkState, val detail: String? = null) : MeshTermView

    /** Link up, handshake with the host under way. A few seconds are ordinary on this link. */
    data object Handshaking : MeshTermView

    /**
     * A handshake has been asked for again and again, and nothing has come back.
     *
     * **A try that keeps failing is a verdict, not a spinner.** The ladder keeps running behind
     * this (a re-ask, then a rebuilt transport), so the link heals by itself when the host comes
     * back — but the person is owed the sentence rather than a counter that climbs for a minute and
     * then says "dropped" about a link that was never up (measured on the device, 2026-11-07: the
     * host radio answered pushes and not commands, and the phone sat on `handshaking · 59 s`).
     */
    data object NoAnswer : MeshTermView

    /** Session established and the page was asked for, but the host has not painted yet. */
    data object ProfileMissing : MeshTermView

    /** No shell was named, so the host was asked which are running and has not finished answering. */
    data object Listing : MeshTermView

    /** The host runs more than one shell and none was named: the user must choose. Nothing is guessed. */
    data class Choose(val entries: List<SessionEntry>) : MeshTermView

    /**
     * Another device holds the shell, and the host refused the page rather than stealing it.
     *
     * This is the mesh route's `HeldBy`: the desk window has it, and the person here can take it
     * back (`Takeover`) or leave it alone. Without this the phone either stole the shell silently
     * or sat on a screen nobody was feeding.
     */
    data class HeldBy(val shell: String) : MeshTermView

    /** The host answered and runs no shell. `refresh` asks again. */
    data object NoShells : MeshTermView

    /** The host described a terminal. [absent] is what this link cannot carry: show it, do not offer it. */
    data class Live(
        val screen: MeshScreen,
        val absent: List<MeshTermFeature>,
        /** Records the radio says may still be sent this hour; null when it has not said. */
        val budgetLeft: Int?,
        /** Something is waiting to be sent and the budget is zero or unreported. */
        val stalled: Boolean,
        /** The mirror is being rebuilt after a gap and still shows the previous screen. */
        val resyncing: Boolean,
        /** True once at least one screen has been applied. Before that the screen is blank. */
        val painted: Boolean,
    ) : MeshTermView

    /** The host ended the page ([reason] is a `mesh_term` `closed` code), or the session ended (null). */
    data class HostClosed(val reason: Int?) : MeshTermView
}

/** The host's shells as the header shows them. [current] is the one attached, when known. */
data class MeshShells(val entries: List<SessionEntry>, val current: String?)

/**
 * What one attempt to send something did.
 *
 * [NotLive] is the rule the driver enforces rather than a guess about the air: input for a shell
 * that is not attached would be written into whatever pane the host happened to have, so it is
 * refused here and reported to the caller.
 */
enum class InputOutcome { Accepted, NotLive, QueueFull, TooLong, Empty }
