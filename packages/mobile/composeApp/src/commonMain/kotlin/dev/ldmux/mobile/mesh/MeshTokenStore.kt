package dev.ldmux.mobile.mesh

/**
 * Where the MeshMore pairing token lives.
 *
 * It is a credential for a local radio gateway, so it goes in platform secure storage and
 * **never** in `AppPrefs`' plain SharedPreferences, which also holds the daemon token and
 * is readable on a rooted device. [load] returns null when there is no token, and also when
 * the stored value cannot be decrypted (a restored backup, a reset keystore): "needs
 * pairing" is the honest state then, not a crash.
 */
interface MeshTokenStore {
    fun load(): ByteArray?
    fun save(token: ByteArray)
    fun clear()
}

/** Tests and previews only: forgets on restart. */
class InMemoryMeshTokenStore(private var token: ByteArray? = null) : MeshTokenStore {
    override fun load(): ByteArray? = token?.copyOf()
    override fun save(token: ByteArray) { this.token = token.copyOf() }
    override fun clear() { token = null }
}
