package io.ldmux.mobile

import io.ldmux.mobile.mesh.MeshLinkState

/**
 * The four sounds a session can make, and the reason there are four rather than a tone.
 *
 * **A cue is information, not a mood.** Three of these mean something an operator acts on without
 * looking at the phone: the link came up (a shell is usable), the link went down (it is not), and
 * something needs attention (a refusal, a stale pairing, an allowance spent). The four themes that
 * have a voice each say them in their own timbre, which is why the set is fixed and small: a theme
 * that invented its own cues would be a theme whose sounds mean nothing to the last theme's user.
 *
 * Dark has none of these, by construction — see [LdmuxTheme.sounds]. It is the quiet option, and
 * silence is a feature rather than an absence of one.
 */
enum class Cue {
    /** A session is live: the handshake finished, the terminal is attached. */
    LinkUp,

    /** The link dropped, or a session it could not build gave up. */
    LinkDown,

    /** Something needs the operator: a refusal, a stale pairing, an allowance that is spent. */
    Attention,
}

/**
 * Whether a cue may sound right now.
 *
 * Two gates that are easy to confuse, so they are stated once: the **theme** decides whether there
 * is a voice at all (Dark has none, and a switch that pretended otherwise would be a switch that
 * does nothing), and the **setting** decides whether this user wants it. Both are read here rather
 * than at each call site, so a new call site cannot forget one of them.
 */
internal fun cueAudible(theme: LdmuxTheme, sounds: Boolean): Boolean = sounds && theme.sounds

/**
 * Play [cue] for [theme] if it is audible.
 *
 * A no-op when the theme is silent or the operator turned sound off: call sites say *when*
 * something happened, never whether to be heard, and a missing platform player (an iOS build, a
 * test) is silence rather than a crash.
 */
internal fun playCue(cue: Cue, theme: LdmuxTheme = ThemeState.theme, sounds: Boolean = CuePrefs.sounds) {
    if (!cueAudible(theme, sounds)) return
    CuePlayer.play(theme, cue)
}

/**
 * The operator's sound switch, mirrored from the persisted config by the root.
 *
 * Mirrored rather than threaded because the callers are one deep in a coroutine (a state flow
 * disappearing) and a boolean that arrives through four composables to be read once is a boolean
 * that will be wrong in one of them. The config is still the source of truth; this is the copy the
 * player needs when there is no composition to read.
 */
internal object CuePrefs {
    @Volatile
    var sounds: Boolean = true
}

/**
 * What a change of the tailnet link means, as a cue or as nothing.
 *
 * Pure, so the mapping is a value a test can hold rather than a rule buried in an effect. Only
 * *reaching* `Online` is a link coming up, and only *leaving* it for a state the person must do
 * something about is a link going down: `Unreachable` retries itself and is worth a tone, but the
 * first `Connecting` of an attempt is not — that would be a tone at every launch and every retry.
 */
internal fun cueForTailnet(from: LinkState, to: LinkState): Cue? = when {
    from == to -> null
    to == LinkState.Online -> Cue.LinkUp
    from == LinkState.Online && to == LinkState.Unreachable -> Cue.LinkDown
    // A refused token is not a dropped link: nothing about waiting fixes it, and the operator has
    // to pair again, which is exactly what "needs attention" is for.
    to == LinkState.Rejected -> Cue.Attention
    else -> null
}

/**
 * The same for the radio route, whose states are the carrier's rather than the socket's.
 *
 * `Bound` is the **radio** answering `APP_START`, not the session being live — a handshake follows
 * it and takes seconds to minutes on a LoRa link — so it is deliberately silent here. The mesh
 * screen knows when a terminal is actually live, and that is its call to make.
 */
internal fun cueForMesh(from: MeshLinkState, to: MeshLinkState): Cue? = when {
    from == to -> null
    to == MeshLinkState.SessionFailed || to == MeshLinkState.RadioUnavailable -> Cue.LinkDown
    to == MeshLinkState.Refused || to == MeshLinkState.NoPath -> Cue.Attention
    else -> null
}

/** The platform player. Android's is a small `SoundPool`; anything else is silence. */
internal expect object CuePlayer {
    fun play(theme: LdmuxTheme, cue: Cue)
}
