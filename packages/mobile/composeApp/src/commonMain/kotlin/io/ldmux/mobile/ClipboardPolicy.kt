package io.ldmux.mobile

/**
 * What a program in the shell may do to the phone's clipboard, as policy.
 *
 * OSC 52 is the shell driving the phone, so it is untrusted input. The platform
 * halves only *carry out* what this lets through, which keeps the policy testable
 * on the JVM and identical on iOS.
 *
 * - **Capped.** Android hands a clip to the system over Binder, whose transaction
 *   buffer is ≈1 MB shared by everything in flight; a program that copies a few
 *   megabytes throws `TransactionTooLargeException` from `setPrimaryClip`, and a
 *   multi-megabyte string on the main thread is a jank before it is an error. Above
 *   the cap the request is **refused whole** rather than truncated: a clipboard that
 *   silently holds half of what the program said is worse than one that is unchanged.
 * - **No control characters.** NUL and the C0 range other than tab/newline/CR make a
 *   clipboard that is dangerous to paste into a terminal later; they are dropped.
 */
const val MAX_CLIPBOARD_CHARS = 100_000

/** The request to carry out, or null when it must be ignored. */
fun admitClipboardRequest(request: ClipboardRequest): ClipboardRequest? = when (request) {
    ClipboardRequest.Clear -> request
    is ClipboardRequest.Set -> {
        if (request.text.length > MAX_CLIPBOARD_CHARS) {
            null
        } else {
            val clean = request.text.filter { it >= ' ' || it == '\n' || it == '\t' || it == '\r' }
                .filter { it != '\u007f' }
            ClipboardRequest.Set(clean)
        }
    }
}
