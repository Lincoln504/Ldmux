package io.ldmux.mobile

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
import android.content.pm.PackageManager
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.util.Log

/**
 * Keeps the process alive for a *bounded* time while a session is open and the app is
 * off screen (ADR-0012).
 *
 * Without this an app switch drops the sockets: Android freezes a backgrounded
 * process and then reclaims it. A foreground service is the sanctioned way to say
 * "this process is doing something the user asked for and knows about", which is
 * true here: the user tapped a shell open.
 *
 * **It is not a promise that the shell stays attached.** The app lets go of every
 * shell once it has been off screen for [BackgroundSession.releaseGraceMs] (45 s),
 * which stops this service. The shell itself lives in the keeper on the host and is
 * unaffected either way; what ends is this phone's *view* and its claim.
 *
 * It holds a **partial** wake lock rather than a screen lock — the CPU may nap, the
 * network does not — with a timeout, so a bug anywhere else cannot keep a phone awake
 * indefinitely; the lock is also released when the service stops.
 */
class LdmuxService : Service() {
    companion object {
        const val ACTION_START = "io.ldmux.mobile.START"
        const val ACTION_STOP = "io.ldmux.mobile.STOP"
        const val EXTRA_TEXT = "text"

        private const val TAG = "ldmux"
        /**
         * The channel of the "keeping the link open" notification, and the id it used to have.
         *
         * A channel's importance is fixed at creation, so the move to the quietest importance
         * (`IMPORTANCE_MIN`) needed a new id. The old one is removed on upgrade, so the phone's
         * notification settings do not list a channel the app no longer posts into.
         */
        private const val CHANNEL_ID = "ldmux.link"
        private const val CHANNEL_ID_OLD = "ldmux.session"
        private const val NOTIFICATION_ID = 1

        /**
         * The longest a wake lock may be held without being renewed. The app releases
         * everything 45 s after leaving the screen, so this is a backstop for a release
         * that never comes (a crashed collector, a lost message), not a working limit;
         * every start command renews it.
         */
        private const val WAKE_LOCK_MAX_MS = 10 * 60 * 1000L

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

    /**
     * Promote to the foreground **before anything else can happen**.
     *
     * `startForegroundService` obliges the service to call `startForeground` within
     * seconds of being created, and the app may call `stopService` right after it (a
     * shell opened and closed in one breath). A service stopped between the two
     * crashed the app with `ForegroundServiceDidNotStartInTime`. Doing it in `onCreate`
     * means the obligation is met by the time the service exists at all, whatever
     * arrives next.
     */
    override fun onCreate() {
        super.onCreate()
        createChannel()
        promote("Keeping the link open")
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        when (intent?.action) {
            ACTION_STOP -> {
                // Let go of the shells, not only the service: the app drops its
                // pages, which closes their sockets and releases their claims.
                // The request is replayed to a collector that is not listening yet,
                // and the service says it is gone so `BackgroundSession` does not
                // believe in a service that no longer exists (it used to, and then
                // never started one again).
                DisconnectRequests.request()
                BackgroundSession.markStopped()
                stopSelf()
                return START_NOT_STICKY
            }
        }
        promote(intent?.getStringExtra(EXTRA_TEXT) ?: "Keeping the link open")
        acquireWakeLock()
        return START_NOT_STICKY
    }

    /** Show (or refresh) the ongoing notification, which is what makes this foreground. */
    private fun promote(text: String) {
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

    /** Take the lock, or renew it: always with a timeout, never indefinitely. */
    private fun acquireWakeLock() {
        val lock = wakeLock ?: run {
            val pm = getSystemService(Context.POWER_SERVICE) as? PowerManager ?: return
            pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "ldmux:session")
                .apply { setReferenceCounted(false) }
                .also { wakeLock = it }
        }
        runCatching { lock.acquire(WAKE_LOCK_MAX_MS) }.onFailure { Log.w(TAG, "wake lock refused", it) }
    }

    private fun releaseWakeLock() {
        wakeLock?.let { if (it.isHeld) runCatching { it.release() } }
        wakeLock = null
    }

    /**
     * Channels and channel-scoped notifications exist from API 26, and this app's
     * `minSdk` is 28 — so there is no old-API branch here to get wrong, and no
     * support library is needed for it.
     */
    private fun createChannel() {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as? NotificationManager ?: return
        runCatching { nm.deleteNotificationChannel(CHANNEL_ID_OLD) }
        // **A channel's name is set once, at creation.** The platform keeps the old text for as
        // long as the id exists, so a rename in the code never reaches the operator's settings
        // screen. Comparing what is there with what this build says, and recreating on a
        // difference, is the only way a name change ships. Recreating resets that channel's user
        // choices, which is why it happens only when the text actually differs.
        val want = NotificationChannel(
            CHANNEL_ID,
            "Connected",
            // **The quietest channel there is.** This notification exists because the platform
            // requires one for a foreground service, not because the operator asked to be told
            // anything: it is posted only while the app is off screen, it makes no sound, shows
            // no icon in the status bar and no badge, and says what is being kept ("2 shells
            // open"). An app that announces itself is an app that gets uninstalled.
            NotificationManager.IMPORTANCE_MIN,
        ).apply {
            description = "While the app keeps a link open in the background"
            setShowBadge(false)
            setSound(null, null)
            enableVibration(false)
            enableLights(false)
        }
        val have = nm.getNotificationChannel(CHANNEL_ID)
        if (have != null && have.name == want.name && have.description == want.description) return
        if (have != null) runCatching { nm.deleteNotificationChannel(CHANNEL_ID) }
        nm.createNotificationChannel(want)
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
            // The app's own mark. A framework drawable read as a system upload glyph on
            // Android 12+, which says nothing about which app is holding the connection.
            .setSmallIcon(R.drawable.ic_notification)
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

    /** The intent extra a mark's notification carries: the shell it is about. */
    const val SHELL_EXTRA = "ldmux.shell"

    private const val MARK_CHANNEL = "ldmux.mark"

    /// Marks get their own id range, apart from the service's single id 1.
    private const val MARK_ID_BASE = 2_000

    /**
     * The service stopped itself (the notification's Disconnect). Without this
     * `running` stayed true and an identical [setActive] call was then ignored, so
     * the service could never be started again until the text happened to change.
     */
    fun markStopped() {
        running = false
        lastText = ""
        // The operator pressed Disconnect: the service must not come back on the next foreground
        // transition. The app asks again on the next `setActive`, which is what opening a shell or
        // changing the route does.
        wantLock = false
        wantText = ""
    }

    /** Held so it is not collected — the framework keeps only a weak reference. */
    private var networkCallback: ConnectivityManager.NetworkCallback? = null

    /**
     * What the link needs, held across a foreground/background change.
     *
     * The platform callbacks re-read it ([applyLockState]), so the service starts at the moment the
     * app leaves the screen and stops at the moment it comes back — which is the point: an ongoing
     * notification that says an app is open is noise to someone looking at the app, and the
     * Activity itself keeps the process alive while it is visible.
     */
    private var wantLock = false
    private var wantText = ""

    /** Whether the app is on screen, from `onResume`/`onPause`: the pair the platform ties to it. */
    private var onScreen = true

    /**
     * Whether anything is being kept alive, i.e. whether a notification would mean something.
     *
     * Read by `MainActivity.onResume` before asking for the notification permission: an app that has
     * never opened a shell has nothing to notify about, and a dialog on first launch is a question
     * about the app rather than about the operator's work.
     */
    fun wantsLock(): Boolean = wantLock

    /** `MainActivity.onPause`: still allowed to start a service, and about to stop being. */
    fun screenPaused() {
        onScreen = false
        applyLockState()
    }

    /** `MainActivity.onResume`: no notification exists for an app the operator is looking at. */
    fun screenResumed() {
        onScreen = true
        applyLockState()
    }

    actual fun setActive(active: Boolean, text: String) {
        wantLock = active
        wantText = text
        applyLockState()
    }

    /**
     * Run the service exactly while the app is **off screen** and the link wants to survive.
     *
     * The foreground service is what keeps Android from freezing the process — a socket, a BLE link
     * and a claim cannot survive that — but it is also a notification the operator cannot dismiss,
     * so it exists only for the time they are not looking: off screen. Coming back stops it, and the
     * text says what is being kept (`2 shells open`), never that the app is running.
     */
    private fun applyLockState() {
        val ctx = AndroidApp.context
        val wanted = wantLock && !onScreen
        if (wanted == running && (wantText == lastText || !wanted)) return
        running = wanted
        lastText = if (wanted) wantText else ""

        val intent = Intent(ctx, LdmuxService::class.java).putExtra(LdmuxService.EXTRA_TEXT, wantText)
        if (!wanted) {
            runCatching { ctx.stopService(intent) }.onFailure { Log.w(TAG, "stopService", it) }
            return
        }
        // The permission ask is not here: `MainActivity.onResume` owns it, because this runs from
        // `onPause` and a system dialog raised from an activity that is leaving is answered by
        // nobody. On Android 13+ a foreground service without the permission still runs, silently,
        // which is the accepted outcome of a denial.
        runCatching {
            // Always the foreground form: minSdk is 28, where it exists.
            ctx.startForegroundService(intent)
        }.onFailure {
            // Refused (the app is not allowed to start one from here): the service is
            // not running, so the next call must try again instead of assuming it is.
            Log.w(TAG, "startForegroundService", it)
            running = false
            lastText = ""
        }
    }

    /**
     * Fires when the set of usable networks *changes*: one becomes validated (not
     * merely present — a wifi with no internet, or a handover in progress, would wake
     * the retry loop for nothing, and a retry storm is how an app gets throttled), or
     * one that was in use is lost or stops validating.
     *
     * The second half is what makes a Wi-Fi to LTE move work. The LTE network was
     * already validated, so nothing "new" appears; what happens is that the Wi-Fi goes,
     * and every socket bound to it is dead without knowing. Reporting the loss is what
     * lets the sockets check themselves at once instead of after three missed pings.
     */
    actual fun watchNetwork(onNetworkChange: () -> Unit) {
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
                val changed = synchronized(validated) {
                    if (ok) validated.add(network) else validated.remove(network)
                }
                if (changed) onNetworkChange()
            }

            override fun onLost(network: Network) {
                val was = synchronized(validated) { validated.remove(network) }
                if (was) onNetworkChange()
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

    /**
     * A shell's mark, as one notification, when the operator asked for notifications.
     *
     * **One channel, and it is not the service's.** The service's own channel exists because the
     * platform demands a notification for a foreground service and is therefore silent and
     * minimal; a mark is the opposite — something the operator asked to be told about — so it posts
     * into its own channel with its own importance.
     *
     * One notification per shell: the id is the shell's hash, so a shell that finishes twice
     * updates its own line instead of stacking. It is a one-off (`setAutoCancel`), never ongoing:
     * the persistent notification in this app is the service's, and there is exactly one of those.
     */
    actual fun postMark(shellId: String, title: String, body: String) {
        // **Never over a page the operator is reading.** The square beside the shell's row is
        // already on screen; a notification on top of the terminal says the same thing twice.
        if (AppLifecycle.foreground) return
        val ctx = AndroidApp.context
        val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as? NotificationManager ?: return
        if (Build.VERSION.SDK_INT >= 33 &&
            ctx.checkSelfPermission(android.Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            // Nothing to post into: the permission is the user's, and the row mark still works.
            return
        }
        ensureMarkChannel(nm)
        val open = PendingIntent.getActivity(
            ctx,
            shellId.hashCode(),
            Intent(ctx, MainActivity::class.java)
                .putExtra(SHELL_EXTRA, shellId)
                .addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP),
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val n = Notification.Builder(ctx, MARK_CHANNEL)
            .setContentTitle(title)
            .setContentText(body)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentIntent(open)
            .setAutoCancel(true)
            .setOnlyAlertOnce(false)
            .setCategory(Notification.CATEGORY_MESSAGE)
            .build()
        // **The whole hash, not twelve bits of it.** Masked to 4096 buckets, two marked shells
        // collided about once in a hundred pairs and one notification silently replaced the other;
        // the id is an int and one shell's line has no reason to share it with another's.
        runCatching { nm.notify(markId(shellId), n) }.onFailure { Log.w(TAG, "notify mark", it) }
    }

    actual fun clearMark(shellId: String) {
        val ctx = AndroidApp.context
        val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as? NotificationManager ?: return
        runCatching { nm.cancel(markId(shellId)) }
    }

    /// One notification per shell, in a range of its own: the service owns id 1.
    private fun markId(shellId: String): Int {
        val h = shellId.hashCode() and 0x7fff_ffff
        return if (h == Int.MAX_VALUE) MARK_ID_BASE else MARK_ID_BASE + h
    }

    private fun ensureMarkChannel(nm: NotificationManager) {
        // Default, because this one is the whole point of the switch: the operator asked to be
        // told that a shell finished a turn, and a notification nobody notices answers a question
        // nobody asked. Turning the switch off is the way to want silence.
        val want = NotificationChannel(
            MARK_CHANNEL,
            "Shell finished or waiting",
            NotificationManager.IMPORTANCE_DEFAULT,
        ).apply {
            description = "A shell finished a turn, or is waiting on you"
            setShowBadge(true)
        }
        val have = nm.getNotificationChannel(MARK_CHANNEL)
        if (have != null && have.name == want.name && have.description == want.description) return
        if (have != null) runCatching { nm.deleteNotificationChannel(MARK_CHANNEL) }
        nm.createNotificationChannel(want)
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
