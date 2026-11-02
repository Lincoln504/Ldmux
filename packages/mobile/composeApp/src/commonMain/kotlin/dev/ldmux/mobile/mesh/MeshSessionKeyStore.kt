package dev.ldmux.mobile.mesh

/**
 * What the L3 session needs that the phone must keep: the phone's own static keypair, the host's
 * public key, and the 32-byte pre-shared secret (ADR-0020). All three are credentials, so they go
 * in platform secure storage with the session keys, never in `AppPrefs`.
 *
 * This only stores and loads them. They arrive by the pairing link (`ldmux code --mesh`):
 * `MeshEnrollment` generates the phone's keypair, takes the host key and the secret from the
 * link, and saves. [MeshSessionKeyStore.hasPeer] is true when the phone holds everything a
 * handshake needs; whether it also knows the host's radio key is [MeshSessionKeys.hostRadio].
 */
class MeshSessionKeys(
    val localPrivate: ByteArray,
    val localPublic: ByteArray,
    val hostPublic: ByteArray,
    val psk: ByteArray,
    /**
     * The host's MeshCore radio public key. Optional because the
     * daemon only knows it when told (`LDMUX_MESH_SELF`); without it the route reports
     * `NeedsPeer`, and nothing guesses one.
     */
    val hostRadio: ByteArray? = null,
) {
    init {
        require(localPrivate.size == KEY && localPublic.size == KEY && hostPublic.size == KEY && psk.size == KEY) {
            "every session key is $KEY bytes"
        }
        require(hostRadio == null || hostRadio.size == KEY) { "the host radio key is $KEY bytes" }
    }

    /**
     * The one blob stored. Version 1 is `[1][localPrivate][localPublic][hostPublic][psk]` (129
     * bytes) and is still what is written when there is no radio key, so an existing install's
     * blob is byte-for-byte what it was. Version 2 appends `[hostRadio]` (161 bytes).
     */
    fun encode(): ByteArray =
        if (hostRadio == null) byteArrayOf(VERSION) + localPrivate + localPublic + hostPublic + psk
        else byteArrayOf(VERSION_WITH_RADIO) + localPrivate + localPublic + hostPublic + psk + hostRadio

    companion object {
        const val KEY = 32
        const val VERSION: Byte = 1
        const val VERSION_WITH_RADIO: Byte = 2
        const val ENCODED = 1 + 4 * KEY
        const val ENCODED_WITH_RADIO = 1 + 5 * KEY

        /** Null for anything that is not exactly a version-1 or version-2 blob: a wrong size is "needs pairing". */
        fun decode(blob: ByteArray?): MeshSessionKeys? {
            if (blob == null || blob.isEmpty()) return null
            val ok = (blob[0] == VERSION && blob.size == ENCODED) ||
                (blob[0] == VERSION_WITH_RADIO && blob.size == ENCODED_WITH_RADIO)
            if (!ok) return null
            fun part(i: Int) = blob.copyOfRange(1 + i * KEY, 1 + (i + 1) * KEY)
            return MeshSessionKeys(part(0), part(1), part(2), part(3), if (blob[0] == VERSION_WITH_RADIO) part(4) else null)
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
