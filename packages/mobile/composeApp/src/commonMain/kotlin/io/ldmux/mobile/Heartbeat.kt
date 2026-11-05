package io.ldmux.mobile

/**
 * How often a client says something to the daemon.
 *
 * One constant for both sockets, because this is a property of the *protocol* and
 * not of either of them. The daemon reaps a connection that has said nothing for
 * 20 s (`WS_IDLE_MS` in `crates/daemon/src/remote.rs`) — which is what makes a phone that
 * vanished without a FIN stop holding shells it is no longer looking at — so every
 * socket the app holds has to speak on a schedule, and "every socket" has to be
 * stated once or it will be stated twice and then disagree once. It did: the
 * per-shell socket pinged and the launcher socket did not, so the launcher would
 * have been reaped every minute and read as the daemon being flaky.
 *
 * Every few seconds is plenty. The interval only has to follow a real change in
 * link quality, and a phone on a metered connection should not spend its budget on
 * keep-alives — each ping is a handful of bytes.
 */
internal const val CLIENT_PING_INTERVAL_MS = 4_000L

/**
 * Unanswered pings before a socket is declared dead, for both sockets alike: the
 * OS only reports a broken link when it notices, and on a sleeping radio or a NAT
 * that forgot the flow it does not notice for a long time.
 */
internal const val CLIENT_MISSED_PONGS = 3

/**
 * How long one heartbeat frame may take to hand to the socket.
 *
 * A write into a socket whose peer and route have both vanished does not fail: on Android it
 * suspends until the kernel buffer drains, which may be never. The heartbeat used to sit inside
 * that suspension, so the pinger never reached its own missed-pong check and the page stayed up
 * and frozen on a link that was long dead (measured 2026-11-03: airplane mode, forty seconds, the
 * terminal still showing a shell it could not reach). Bounding the write lets the beat happen.
 */
internal const val CLIENT_PING_SEND_TIMEOUT_MS = 2_000L
