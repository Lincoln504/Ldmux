package dev.ldmux.mobile.mesh

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.content.pm.PackageManager
import android.os.DeadObjectException
import android.os.IBinder
import android.os.Parcel
import android.os.RemoteException
import dev.ldmux.mobile.AndroidApp
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeoutOrNull

private const val MESHMORE_PACKAGE = "com.iotone.meshmore_sns_app"
private const val MESHMORE_SERVICE = "$MESHMORE_PACKAGE.GatewayHandoffService"
private const val DESCRIPTOR = "$MESHMORE_PACKAGE.GatewayHandoff"
private const val TRANSACTION_GET = IBinder.FIRST_CALL_TRANSACTION

/** Long enough to start a cold MeshMore process, short enough that a button press is not a hang. */
private const val BIND_TIMEOUT_MS = 4_000L

actual suspend fun requestMeshHandoff(): MeshHandoffResult = readMeshMoreHandoff(AndroidApp.context)

/**
 * Bind to MeshMore's handoff service by an **explicit** component, read one offer, and unbind.
 *
 * Explicit because an implicit bind is refused outright from API 21, and because naming the
 * component is the point: this asks one specific app, and the platform's signature permission (the
 * `uses-permission` in this app's manifest against the `permission` MeshMore declares) decides
 * whether it may. Every failure is a [MeshHandoffResult.Failed], never an exception, and the
 * connection never outlives the call.
 */
internal suspend fun readMeshMoreHandoff(context: Context): MeshHandoffResult {
    try {
        context.packageManager.getPackageInfo(MESHMORE_PACKAGE, 0)
    } catch (_: PackageManager.NameNotFoundException) {
        return MeshHandoffResult.Failed(MeshHandoffFailure.NotInstalled)
    }

    val binder = CompletableDeferred<IBinder?>()
    val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, service: IBinder) { binder.complete(service) }
        override fun onServiceDisconnected(name: ComponentName) { binder.complete(null) }
        override fun onNullBinding(name: ComponentName) { binder.complete(null) }
        override fun onBindingDied(name: ComponentName) { binder.complete(null) }
    }
    val intent = Intent().setComponent(ComponentName(MESHMORE_PACKAGE, MESHMORE_SERVICE))
    val bound = try {
        context.bindService(intent, connection, Context.BIND_AUTO_CREATE)
    } catch (_: SecurityException) {
        return MeshHandoffResult.Failed(MeshHandoffFailure.PermissionDenied)
    }
    // `false` with the package present means there is no such service in it: an older MeshMore.
    if (!bound) {
        runCatching { context.unbindService(connection) }
        return MeshHandoffResult.Failed(MeshHandoffFailure.ServiceMissing)
    }
    try {
        val service = withTimeoutOrNull(BIND_TIMEOUT_MS) { binder.await() }
            ?: return MeshHandoffResult.Failed(MeshHandoffFailure.Timeout)
        // The transaction blocks, so it is not made on the main thread the connection callback ran on.
        return withContext(Dispatchers.IO) { transact(service) }
    } finally {
        runCatching { context.unbindService(connection) }
    }
}

private fun transact(service: IBinder): MeshHandoffResult {
    val data = Parcel.obtain()
    val reply = Parcel.obtain()
    try {
        data.writeInterfaceToken(DESCRIPTOR)
        service.transact(TRANSACTION_GET, data, reply, 0)
        reply.readException()
        val b = reply.readBundle(MeshHandoffOffer::class.java.classLoader)
            ?: return MeshHandoffResult.Failed(MeshHandoffFailure.BadReply)
        return meshHandoffOf(
            version = b.getInt("version", 0),
            state = b.getString("state"),
            port = b.getInt("port", 0),
            token = b.getByteArray("token"),
            radioKey = b.getString("radioKey"),
            peerName = b.getString("peerName"),
        )
    } catch (_: SecurityException) {
        // The service's own check (the second lock behind the manifest's) said no.
        return MeshHandoffResult.Failed(MeshHandoffFailure.PermissionDenied)
    } catch (_: DeadObjectException) {
        return MeshHandoffResult.Failed(MeshHandoffFailure.Timeout)
    } catch (_: RemoteException) {
        return MeshHandoffResult.Failed(MeshHandoffFailure.BadReply)
    } catch (_: RuntimeException) {
        // A malformed parcel: unreadable, not fatal.
        return MeshHandoffResult.Failed(MeshHandoffFailure.BadReply)
    } finally {
        data.recycle()
        reply.recycle()
    }
}
