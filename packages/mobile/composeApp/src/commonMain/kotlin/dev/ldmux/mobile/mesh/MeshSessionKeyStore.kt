package dev.ldmux.mobile.mesh

/**
 * What the L3 session needs that the phone must keep: the phone's own static keypair, the host's
 * public key, and the 32-byte pre-shared secret (ADR-0020). All three are credentials, so they go
 * in platform secure storage with the pairing token, never in `AppPrefs`.
 *
 * This only stores and loads them. **How they arrive is not built**: the QR version 2 ceremony
 * that carries the host key and the secret to the phone is still missing, so nothing calls
 * [MeshSessionKeyStore.save] outside tests, and [hasPeer] is true only after something does.
 */
class MeshSessionKeys(
    val localPrivate: ByteArray,
    val localPublic: ByteArray,
    val hostPublic: ByteArray,
    val psk: ByteArray,
) {
    init {
        require(localPrivate.size == KEY && localPublic.size == KEY && hostPublic.size == KEY && psk.size == KEY) {
            "every session key is $KEY bytes"
        }
    }

    /** The one blob stored: `[version][localPrivate][localPublic][hostPublic][psk]`. */
    fun encode(): ByteArray = byteArrayOf(VERSION) + localPrivate + localPublic + hostPublic + psk

    companion object {
        const val KEY = 32
        const val VERSION: Byte = 1
        const val ENCODED = 1 + 4 * KEY

        /** Null for anything that is not exactly a version-1 blob: a wrong size is "needs pairing". */
        fun decode(blob: ByteArray?): MeshSessionKeys? {
            if (blob == null || blob.size != ENCODED || blob[0] != VERSION) return null
            fun part(i: Int) = blob.copyOfRange(1 + i * KEY, 1 + (i + 1) * KEY)
            return MeshSessionKeys(part(0), part(1), part(2), part(3))
        }
    }
}

interface MeshSessionKeyStore {
    /** Null when nothing is stored, and when what is stored cannot be read back. */
    fun load(): MeshSessionKeys?
    fun save(keys: MeshSessionKeys)
    fun clear()

    /** The route resolver's `hasPeer`: the phone holds everything a handshake needs. */
    val hasPeer: Boolean get() = load() != null
}

/** Tests and previews only: forgets on restart. */
class InMemoryMeshSessionKeyStore(private var blob: ByteArray? = null) : MeshSessionKeyStore {
    override fun load(): MeshSessionKeys? = MeshSessionKeys.decode(blob)
    override fun save(keys: MeshSessionKeys) { blob = keys.encode() }
    override fun clear() { blob = null }
}
