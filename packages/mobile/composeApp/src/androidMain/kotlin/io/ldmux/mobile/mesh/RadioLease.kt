package io.ldmux.mobile.mesh

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import io.ldmux.mobile.appLog
import android.os.Build
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

/**
 * **One radio, one app at a time — and Android will not arbitrate it.**
 *
 * A MeshCore BLE companion accepts exactly **one** central, by firmware design. Community tooling
 * states it plainly ("MeshCore only supports one BLE connection at a time") and the failure shape is
 * the worst kind: the second client often *completes* the connection and is dropped later, when the
 * first client next writes. There is no Android API that lets one app close another app's GATT link,
 * so the only workable arrangement is an agreement between the apps.
 *
 * That is what this is: ldmux announces that it wants the radio, any counterpart that is holding it
 * disconnects and answers, and ldmux announces when it is done. Both sides treat the other's absence
 * as "the radio is free", because an app that is not installed, or was killed, holds nothing.
 *
 * Measured on 2026-11-10 on the phone: the other MeshCore app was registering a GATT client,
 * connecting, and dropping the link every thirty seconds (three `DISCONNECTED … reason=22` events in
 * one minute), while ldmux was mid-handshake. From the operator's side that reads as "handshake,
 * fail, handshake, fail, sometimes the link dropped" — which is exactly what they reported.
 *
 * The action names are the contract; [COUNTERPARTS] is the list of apps known to hold this radio.
 * Any other app can join by answering the same actions.
 */
internal object RadioLease {
    const val ACTION_WANTED = "io.ldmux.mobile.action.RADIO_WANTED"
    const val ACTION_FREE = "io.ldmux.mobile.action.RADIO_FREE"
    const val ACTION_GRANTED = "io.ldmux.mobile.action.RADIO_GRANTED"

    /** Apps that take the same physical radio. Add one here and it is asked like the others. */
    val COUNTERPARTS = listOf("com.iotone.meshmore_sns_app")

    /**
     * How long a holder is given to let go.
     *
     * The holder's job is a `gatt.disconnect()` plus a few hundred milliseconds of firmware
     * bookkeeping, and a disconnect the firmware is slow to confirm costs the requester a whole
     * connection attempt against a radio that is still busy. Three seconds is generous for that and
     * short enough that a counterpart which is not running — and therefore answers nothing — does
     * not visibly delay the connect.
     */
    private const val ANSWER_MS = 3_000L

    /**
     * Ask, then wait a moment for the answer.
     *
     * Returns true when a counterpart granted, false when none answered (which is the normal case
     * with the radio already free). A refusal is logged and treated as "not granted" — the connect
     * still proceeds, because a link that is attempted and fails names its own reason, where a
     * connect that never happened explains nothing.
     */
    suspend fun acquire(context: Context): Boolean = withContext(Dispatchers.Main.immediate) {
        val answer = CompletableDeferred<Boolean>()
        val receiver = object : BroadcastReceiver() {
            override fun onReceive(c: Context?, intent: Intent?) {
                if (intent?.action == ACTION_GRANTED) answer.complete(true)
            }
        }
        val filter = IntentFilter(ACTION_GRANTED)
        val listening = runCatching {
            // Exported: the answer comes from another app, and an unexported registration would drop
            // it with no trace on either side. The flag exists from API 33; below it the platform
            // has no such distinction and the two-argument overload is the whole story.
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                context.registerReceiver(receiver, filter, Context.RECEIVER_EXPORTED)
            } else {
                @Suppress("UnspecifiedRegisterReceiverFlag")
                context.registerReceiver(receiver, filter)
            }
        }.isSuccess
        if (!listening) {
            appLog("mesh radio lease: cannot listen for an answer")
            return@withContext false
        }
        try {
            var asked = 0
            for (pkg in COUNTERPARTS) {
                val wanted = Intent(ACTION_WANTED)
                    .setPackage(pkg)
                    .putExtra("requested_by", context.packageName)
                // An explicit broadcast reaches a manifest receiver in a background app, which an
                // implicit one no longer does on Android 8+.
                if (runCatching { context.sendBroadcast(wanted) }.isSuccess) asked++
            }
            if (asked == 0) return@withContext false
            withTimeoutOrNull(ANSWER_MS) { answer.await() } ?: false
        } finally {
            runCatching { context.unregisterReceiver(receiver) }
        }
    }

    /** Tell anyone waiting that the radio is free again. */
    fun release(context: Context) {
        for (pkg in COUNTERPARTS) {
            runCatching {
                context.sendBroadcast(
                    Intent(ACTION_FREE).setPackage(pkg).putExtra("freed_by", context.packageName),
                )
            }
        }
        appLog("mesh radio lease: released")
    }
}
