package dev.ldmux.mobile.mesh

import android.content.Context

/**
 * The MeshMore pairing token in the Android Keystore (see [KeystoreBlob]). Preference key
 * `mesh_token_v1` and the Keystore alias are unchanged from before the extraction, so a token
 * saved by an earlier build still loads.
 */
class KeystoreMeshTokenStore(context: Context) : MeshTokenStore {
    private val blob = KeystoreBlob(context, "mesh_token_v1")
    override fun load(): ByteArray? = blob.load()
    override fun save(token: ByteArray) = blob.save(token)
    override fun clear() = blob.clear()
}

/** The L3 session keys in the Android Keystore (see [MeshSessionKeys]). */
class KeystoreMeshSessionKeyStore(context: Context) : MeshSessionKeyStore {
    private val blob = KeystoreBlob(context, "mesh_session_keys_v1")
    override fun load(): MeshSessionKeys? = MeshSessionKeys.decode(blob.load())
    override fun save(keys: MeshSessionKeys) = blob.save(keys.encode())
    override fun clear() = blob.clear()
}
