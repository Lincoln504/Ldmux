package dev.ldmux.mobile

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log

/**
 * Keeps the process alive while a session is open.
 *
 * Without this the daemon link is a foreground-only feature. Android freezes a
 * backgrounded process and then reclaims it, so a phone in a pocket drops the
 * socket and, with it, the illusion that the terminal is still there. A
 * foreground service is the sanctioned way to say "this process is doing
 * something the user asked for and knows about", which is exactly true here: the
 * user tapped a shell open.
 *
 * It holds a **partial** wake lock rather than a screen lock — the CPU may nap,
 * the network does not — and it releases it the instant the link drops, because
 * a terminal sitting idle is not a reason to keep a phone awake.
 */
class LdmuxService : Service() {
    companion object {
        const val ACTION_START = "dev.ldmux.mobile.START"
        const val ACTION_STOP = "dev.ldmux.mobile.STOP"
        const val EXTRA_TEXT = "text"

        private const val TAG = "ldmux"
        private const val CHANNEL_ID = "ldmux.session"
        private const val NOTIFICATION_ID = 1

        /**
         * Notifications are needed before the service can be told to run, and
         * only an activity can ask. Set from `MainActivity`, cleared on destroy,
         * so the ask happens while the user is looking at the app rather than
         * never.
         */
        @Volatile
        var activity: MainActivity? = null
    }

    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                // Let go of the shells, not only the service: the app drops its
                // pages, which closes their sockets and releases their claims, and
                // that in turn tells this service to stop.
                DisconnectRequests.request()
                stopSelf()
                return START_NOT_STICKY
            }
        }
        val text = intent?.getStringExtra(EXTRA_TEXT) ?: "Session open"
        createChannel()
        val n = notification(text)
        // Android 14 takes the type explicitly; it must match the manifest, or
        // the service is refused at the moment the user is watching. Below 14 the
        // two-argument form is the only one that exists.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            startForeground(NOTIFICATION_ID, n, BackgroundSession.foregroundServiceType)
        } else {
            @Suppress("DEPRECATION")
            startForeground(NOTIFICATION_ID, n)
        }
        acquireWakeLock()
        return START_NOT_STICKY
    }

    /**
     * Never sticky. A restarted service would come back with no connection to
     * keep alive, and a notification claiming a session is open when it is not
     * is worse than no notification.
     */
    override fun onDestroy() {
        releaseWakeLock()
        super.onDestroy()
    }

    private fun acquireWakeLock() {
        if (wakeLock != null) return
        val pm = getSystemService(Context.POWER_SERVICE) as? PowerManager ?: return
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "ldmux:session").apply {
            setReferenceCounted(false)
            runCatching { acquire() }.onFailure { Log.w(TAG, "wake lock refused", it) }
        }
    }

    private fun releaseWakeLock() {
        wakeLock?.let { if (it.isHeld) runCatching { it.release() } }
        wakeLock = null
    }

    /**
     * Channels and channel-scoped notifications exist from API 26, which is this
     * app's `minSdk` — so there is no old-API branch here to get wrong, and no
     * support library is needed for it.
     */
    private fun createChannel() {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (nm.getNotificationChannel(CHANNEL_ID) != null) return
        nm.createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                "Open sessions",
                // Low: this is a status light for something the user started, not
                // something asking for attention. A terminal that buzzes is a
                // terminal that gets uninstalled.
                NotificationManager.IMPORTANCE_LOW,
            ).apply {
                description = "Shown while a shell is connected"
                setShowBadge(false)
            },
        )
    }

    private fun notification(text: String): Notification {
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val stop = PendingIntent.getService(
            this,
            1,
            Intent(this, LdmuxService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("ldmux")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_sys_upload_done)
            .setContentIntent(open)
            .addAction(Notification.Action.Builder(null, "Disconnect", stop).build())
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(Notification.CATEGORY_SERVICE)
            .build()
    }
}

/**
 * The Android half of [BackgroundSession].
 *
 * The service is started and stopped here rather than from the shared code so
 * that the only thing crossing the seam is a boolean: "the link is up".
 */
actual object BackgroundSession {
    actual val releaseGraceMs: Long = 45_000L

    private const val TAG = "ldmux"
    private var running = false
    private var lastText = ""

    /** Held so it is not collected — the framework keeps only a weak reference. */
    private var networkCallback: ConnectivityManager.NetworkCallback? = null

    actual fun setActive(active: Boolean, text: String) {
        val ctx = AndroidApp.context
        if (active == running && text == lastText) return
        running = active
        lastText = text

        val intent = Intent(ctx, LdmuxService::class.java).putExtra(LdmuxService.EXTRA_TEXT, text)
        if (!active) {
            runCatching { ctx.stopService(intent) }.onFailure { Log.w(TAG, "stopService", it) }
            return
        }
        // Ask for the notification permission first: on Android 13+ a foreground
        // service without it still runs, but silently, and a service the user
        // cannot see is a service the user cannot stop.
        LdmuxService.activity?.requestNotificationPermissionIfNeeded()
        runCatching {
            // Always the foreground form: minSdk is 26, where it exists.
            ctx.startForegroundService(intent)
        }.onFailure { Log.w(TAG, "startForegroundService", it) }
    }

    /**
     * Fires when a network becomes *validated*, not merely present.
     * A phone that has joined a wifi with no internet, or is mid-handover, would
     * otherwise wake the retry loop for nothing — and a retry storm during a
     * handover is how an app gets throttled by the OS.
     */
    actual fun watchNetwork(onNetworkUp: () -> Unit) {
        val ctx = AndroidApp.context
        val cm = ctx.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager ?: return
        if (networkCallback != null) return
        val cb = object : ConnectivityManager.NetworkCallback() {
            /** Networks already reported as validated, so each wakes once. */
            private val validated = mutableSetOf<Network>()

            // Validation is usually reported *after* `onAvailable`, through a
            // capabilities change — checking only in `onAvailable` missed most
            // Wi-Fi rejoins. So the transition to validated is what fires.
            override fun onCapabilitiesChanged(network: Network, caps: NetworkCapabilities) {
                val ok = caps.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
                val fresh = synchronized(validated) { if (ok) validated.add(network) else { validated.remove(network); false } }
                if (fresh) onNetworkUp()
            }

            override fun onLost(network: Network) {
                synchronized(validated) { validated.remove(network) }
            }
        }
        networkCallback = cb
        runCatching {
            cm.registerNetworkCallback(
                NetworkRequest.Builder()
                    .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                    // A request excludes VPNs unless told otherwise, and Tailscale —
                    // the network this app actually reaches the host over — is one.
                    .removeCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
                    .build(),
                cb,
            )
        }.onFailure { Log.w(TAG, "registerNetworkCallback", it) }
    }

    actual fun stopWatchingNetwork() {
        val cb = networkCallback ?: return
        networkCallback = null
        val cm = AndroidApp.context.getSystemService(Context.CONNECTIVITY_SERVICE) as? ConnectivityManager
        runCatching { cm?.unregisterNetworkCallback(cb) }
    }

    /** The foreground-service type this app declares in its manifest. */
    val foregroundServiceType: Int
        get() = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
        } else {
            0
        }
}
