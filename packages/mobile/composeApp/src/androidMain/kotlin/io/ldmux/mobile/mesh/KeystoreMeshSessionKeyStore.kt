package io.ldmux.mobile.mesh

import android.content.Context

/** The L3 session keys in the Android Keystore (see [MeshSessionKeys]). */
class KeystoreMeshSessionKeyStore(context: Context) : MeshSessionKeyStore {
    private val blob = KeystoreBlob(context, "mesh_session_keys_v1")
    override fun load(): MeshSessionKeys? = MeshSessionKeys.decode(blob.load())
    override fun save(keys: MeshSessionKeys) = blob.save(keys.encode())
    override fun clear() = blob.clear()
}
