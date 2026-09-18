package dev.ldmux.mobile

/**
 * What the app needs from the operating system to stay connected while it is not
 * on screen.
 *
 * A remote terminal is useless if the link dies the moment you switch to another
 * app, and on Android that is the default: a backgrounded process is frozen and
 * then killed, socket and all. Keeping it alive is a platform concern, so it is
 * behind this seam and the shared code only states the intent.
 *
 * Deliberately not a "start a service" API. The shared layer says *the link is
 * up* or *the link is down*; whether that means a foreground service, a wake
 * lock, or nothing at all (iOS has no equivalent and needs none) is the
 * platform's business.
 */
expect object BackgroundSession {
    /**
     * How long the app may be off screen before it lets go of its shells.
     *
     * **A platform number, not a shared one.** Android can hold a process alive
     * and keep a link up, so a grace window is a real window during which nothing
     * needs to happen. iOS cannot: the process is suspended within seconds and a
     * task that outlives the background window is killed, so a coroutine delay
     * there is a detach that never runs. iOS answers 0 and releases synchronously
     * in `didEnterBackground`.
     */
    val releaseGraceMs: Long

    /**
     * The link went up (with [text] describing what is open) or came down.
     *
     * Called on every change, and only on change — this starts and stops a
     * foreground service, and a service told it is active when it already is
     * must not flash its notification.
     */
    fun setActive(active: Boolean, text: String)

    /**
     * Ask to be told the moment a usable network appears, so the client can
     * retry then rather than waiting out its backoff.
     */
    fun watchNetwork(onNetworkUp: () -> Unit)

    /** Stop watching — the app is going away. */
    fun stopWatchingNetwork()
}
