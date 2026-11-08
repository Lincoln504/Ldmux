package io.ldmux.mobile

import io.ktor.client.HttpClientConfig
import io.ktor.client.plugins.websocket.WebSockets
import io.ktor.websocket.WebSocketDeflateExtension

/**
 * The WebSocket client plugin, configured the one way this app wants it.
 *
 * Both of the app's sockets used to build this independently — `install(WebSockets)`
 * written twice, and nothing else — which is how they could drift. They cannot now.
 *
 * **Compression is the point of this function.** The daemon compresses a message only
 * at 96 bytes or more (`crates/daemon/src/ws.rs`, ADR-0019), because a keystroke echo is ≈9 bytes and deflate's own
 * framing would make it bigger. Repetition in terminal output, on the other hand, is
 * enormous — `ls -laR` was measured at 421 KB, 7.1x smaller deflated — and it recurs *across* messages, which
 * is why context takeover is left on: a terminal's output is a stream, and a stream
 * compresses better when the compressor remembers what came before.
 *
 * The matching server side is the daemon's own permessage-deflate (`crates/daemon/src/ws.rs`,
 * ADR-0019), negotiated only when this client offers it. Neither half does anything without the other, which is the whole reason
 * to keep them described in one place.
 */
internal fun HttpClientConfig<*>.ldmuxWebSockets() {
    install(WebSockets) {
        extensions {
            install(WebSocketDeflateExtension)
        }
    }
}
