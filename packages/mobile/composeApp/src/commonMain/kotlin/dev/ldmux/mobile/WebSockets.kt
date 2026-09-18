package dev.ldmux.mobile

import io.ktor.client.HttpClientConfig
import io.ktor.client.plugins.websocket.WebSockets
import io.ktor.websocket.WebSocketDeflateExtension

/**
 * The WebSocket client plugin, configured the one way this app wants it.
 *
 * Both of the app's sockets used to build this independently — `install(WebSockets)`
 * written twice, and nothing else — which is how they could drift. They cannot now.
 *
 * **Compression is the point of this function.** The daemon compresses a frame only
 * above 512 bytes, because a keystroke echo is about 9 bytes and deflate's own
 * framing would make it bigger. Repetition in terminal output, on the other hand, is
 * enormous — `ls -laR` was measured at 515 KB — and it recurs *across* messages, which
 * is why context takeover is left on: a terminal's output is a stream, and a stream
 * compresses better when the compressor remembers what came before.
 *
 * The matching server side is `perMessageDeflate` in `remote.ts` with the same
 * threshold. Neither half does anything without the other, which is the whole reason
 * to keep them described in one place.
 */
internal fun HttpClientConfig<*>.ldmuxWebSockets() {
    install(WebSockets) {
        extensions {
            install(WebSocketDeflateExtension)
        }
    }
}
