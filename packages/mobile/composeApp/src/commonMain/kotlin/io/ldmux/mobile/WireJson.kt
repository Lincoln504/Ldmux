package io.ldmux.mobile

import io.ktor.client.HttpClient
import io.ktor.client.request.get
import io.ktor.client.request.header
import io.ktor.http.HttpHeaders
import kotlinx.coroutines.withTimeoutOrNull
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.add
import kotlinx.serialization.json.putJsonArray
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject

/**
 * The one set of tolerant JSON readers, and the pieces of the wire both sockets share.
 *
 * These existed three times (`AttachClient.field`, `LdmuxClient`'s private extension
 * set, and the throwing `jsonPrimitive` calls the second file's own comments said had
 * been removed). `jsonPrimitive` and `jsonObject` **throw** on a `JsonNull` or a value of
 * the wrong shape, and a safe-call only guards an *absent* key. Every accessor here
 * answers "nothing" instead, so one unexpected field from a newer daemon costs one
 * field, never a message and never the socket.
 */

private val lenient = Json { ignoreUnknownKeys = true }

/** The text as a JSON object, or null when it is not valid JSON or not an object. */
internal fun parseJsonObject(text: String): JsonObject? =
    try {
        lenient.parseToJsonElement(text) as? JsonObject
    } catch (_: Exception) {
        null
    }

internal fun JsonObject.text(key: String): String =
    ((this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull })?.content ?: ""

internal fun JsonObject.optionalText(key: String): String? =
    ((this[key] as? JsonPrimitive)?.takeIf { it !is JsonNull })?.content

internal fun JsonObject.number(key: String): Int? = text(key).toIntOrNull()

internal fun JsonObject.flag(key: String): Boolean = text(key) == "true"

internal fun JsonObject.obj(key: String): JsonObject? = this[key] as? JsonObject

internal fun JsonObject.objects(key: String): List<JsonObject> =
    (this[key] as? JsonArray)?.mapNotNull { it as? JsonObject } ?: emptyList()

/**
 * Who we are, said first on every socket (ADR-0010).
 *
 * Built in one place: the session-list client and the per-shell client each built
 * this by hand, so the two could disagree about what a phone announces.
 */
internal fun helloMessage(config: LdmuxConfig, features: List<String> = emptyList()): String =
    buildJsonObject {
        put("type", "hello")
        putJsonObject("device") {
            put("id", config.deviceId)
            put("name", config.deviceLabel)
            put("kind", "phone")
        }
        if (features.isNotEmpty()) {
            // What this client understands, so the daemon can answer additively. A
            // client that says nothing gets exactly the messages it always got.
            putJsonArray("features") { features.forEach { add(it) } }
        }
    }.toString()

/**
 * The features an attached page advertises.
 *
 * `inplaceResync` is the one that matters to a phone: without it, a client that
 * falls behind its socket is repaired by **closing the connection** — which costs
 * a reconnect (seconds of the user's life, and the one event that makes ownership
 * look like it changed hands), an attach, a fresh snapshot and a rebuilt engine.
 * With it the daemon sends a `caps` + `snapshot` marked `resync` on the same
 * socket, which the client already applies in place. That is the same repair for a
 * fraction of the bytes: it is the one lever `docs/ARCHITECTURE.md#9-airtime` measures at two
 * orders of magnitude.
 */
internal val PAGE_FEATURES: List<String> = listOf("inplaceResync")

/**
 * What a client that wants the shell marks says in `hello.features`.
 *
 * A client that says nothing receives nothing new: the marks are an addition to the wire, not a
 * change to it. The presence client asks for this, and so does a page, because a page clears its
 * shell's mark by being opened and must therefore know the table.
 */
internal const val FEATURE_BUZZ = "buzz"

/**
 * What the daemon said to a plain `GET /ws`, or why it said nothing.
 *
 * Bounded, because a machine that is not there does not say so quickly. Shared so a terminal page
 * can tell a refused token from a machine that is off, the same way the launcher socket does (see
 * [linkStateFromProbe]).
 *
 * **"Nothing answered" was one answer for three different problems**, and they have three
 * different fixes: a host that is up with nothing listening (the daemon is not running), a host
 * that never answered (the address is wrong, or the tailnet is not connected), and a reply that was
 * not a status at all. The first is the one worth naming, because it is the one the person can fix
 * on the machine they are pairing with.
 */
internal sealed interface ProbeAnswer {
    /** A status, so the daemon answered. */
    data class Answer(val status: Int) : ProbeAnswer
    /** The host refused the connection: it is reachable and nothing is listening on that port. */
    data object Refused : ProbeAnswer
    /** No answer in time, or no route to ask on. */
    data object NoAnswer : ProbeAnswer
}

internal suspend fun probeLink(http: HttpClient, endpoint: Endpoint): ProbeAnswer =
    withTimeoutOrNull(PROBE_TIMEOUT_MS) {
        try {
            ProbeAnswer.Answer(
                http.get(endpoint.probeUrl) { header(HttpHeaders.Authorization, endpoint.authorization) }.status.value,
            )
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (e: Exception) {
            if (probeFailureIsRefused(e)) ProbeAnswer.Refused else ProbeAnswer.NoAnswer
        }
    } ?: ProbeAnswer.NoAnswer

/**
 * Whether [e] is the host refusing the connection rather than failing to answer.
 *
 * Per-platform because the answer is a socket errno and every platform spells it differently:
 * Android's refuses with `java.net.ConnectException` ("Failed to connect to …: Connection
 * refused"), and a timeout is `SocketTimeoutException` or Ktor's own timeout type.
 */
internal expect fun probeFailureIsRefused(e: Throwable): Boolean
