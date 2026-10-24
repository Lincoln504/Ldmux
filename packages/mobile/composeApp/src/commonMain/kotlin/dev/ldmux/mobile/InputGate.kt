package dev.ldmux.mobile

/**
 * Whether input may be sent, and whether it may be *shown* before the server echoes it.
 *
 * Input while the link was down was painted by the predictor and then silently lost:
 * the user watched a command appear at the prompt that no shell would ever receive. A
 * terminal that cannot send must say so and must not pretend.
 */
data class InputGate(
    /** The bytes can go to a live socket. When false they are refused, not queued. */
    val canSend: Boolean,
    /** A prediction may be drawn. Never true when [canSend] is false. */
    val canPredict: Boolean,
)

/**
 * The gate, from the facts that decide it.
 *
 * Refused rather than queued: a keystroke or a paste held across a reconnect would be
 * delivered at a moment nobody meant it for, into whatever the prompt has become.
 *
 * - [connected]: the socket is up.
 * - [heldElsewhere]: another device owns the shell, so the daemon would refuse input
 *   and a prediction would be painted and then withdrawn.
 * - [predict], [alternateScreen], [passwordPrompt]: the prediction gates.
 *   [predict] is not a setting: it is the link being slow enough that painted text
 *   is better than waiting (see [FramePacing.predict]).
 */
fun inputGate(
    connected: Boolean,
    heldElsewhere: Boolean,
    predict: Boolean,
    alternateScreen: Boolean,
    passwordPrompt: Boolean,
): InputGate {
    val send = connected && !heldElsewhere
    return InputGate(
        canSend = send,
        canPredict = send && predict && !alternateScreen && !passwordPrompt,
    )
}
