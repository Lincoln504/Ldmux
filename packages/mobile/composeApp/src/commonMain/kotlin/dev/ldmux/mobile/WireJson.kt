package dev.ldmux.mobile

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

internal fun JsonObject.objects(key: String): List<JsonObject> =
    (this[key] as? JsonArray)?.mapNotNull { it as? JsonObject } ?: emptyList()

/**
 * Who we are, said first on every socket (ADR-0010).
 *
 * Built in one place: the session-list client and the per-shell client each built
 * this by hand, so the two could disagree about what a phone announces.
 */
internal fun helloMessage(config: LdmuxConfig): String = buildJsonObject {
    put("type", "hello")
    putJsonObject("device") {
        put("id", config.deviceId)
        put("name", config.deviceLabel)
        put("kind", "phone")
    }
}.toString()

/**
 * The daemon's HTTP status for a plain `GET /ws`, or null when nothing answered.
 * Bounded, because a machine that is not there does not say so quickly. Shared so a
 * terminal page can tell a refused token from a machine that is off, the same way
 * the launcher socket does (see [linkStateFromProbe]).
 */
internal suspend fun probeLinkStatus(http: HttpClient, endpoint: Endpoint): Int? =
    withTimeoutOrNull(PROBE_TIMEOUT_MS) {
        try {
            http.get(endpoint.probeUrl) { header(HttpHeaders.Authorization, endpoint.authorization) }.status.value
        } catch (e: kotlinx.coroutines.CancellationException) {
            throw e
        } catch (_: Exception) {
            null
        }
    }
