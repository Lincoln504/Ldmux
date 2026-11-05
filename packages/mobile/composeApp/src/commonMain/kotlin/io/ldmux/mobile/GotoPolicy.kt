package io.ldmux.mobile

/** How long a "go to the shell I just created" request may stay outstanding. */
internal const val GOTO_TIMEOUT_MS = 10_000L

/**
 * What to do with an outstanding request to navigate to a shell the daemon just made.
 *
 * The request used to be cleared only when the shell became an attachable page. A
 * shell that was created and then died or never listed left it set forever, and an
 * outstanding request blocks the "none of my shells is left, back to the list" rule —
 * a navigation stall on an empty terminal screen.
 */
enum class GotoState {
    /** Keep waiting: the session list has not carried it yet. */
    Wait,

    /** It is a page now; navigate and clear. */
    Arrive,

    /** It can never arrive (ended, or the link dropped); clear without navigating. */
    Abandon,
}

fun gotoState(want: String, sessions: List<Session>, pageIds: List<String>, connected: Boolean): GotoState {
    if (want in pageIds) return GotoState.Arrive
    if (!connected) return GotoState.Abandon
    // Listed as dead is a shell that is gone, not one on its way. (Merely not yet
    // attachable — no keeper name yet — is `starting`, and is waited for.)
    if (sessions.any { it.id == want && it.status == "dead" }) return GotoState.Abandon
    return GotoState.Wait
}
