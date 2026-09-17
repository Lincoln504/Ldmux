package dev.ldmux.mobile

/**
 * A stable identity for this install, and what to call it (ADR-0010).
 *
 * Generated once and persisted by `AppPrefs`. It is deliberately *not* a
 * credential: nothing is authorised by it, and a device that lies about its id
 * only manages to be told apart wrongly. It exists so that "open on Pixel 8" can
 * be a sentence, and so a claim can be attributed to the connection that made it.
 */
expect fun newDeviceId(): String

/** What a human calls this device in "open on …". */
expect fun deviceLabel(): String
