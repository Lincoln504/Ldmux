package dev.ldmux.mobile.mesh

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * The MeshMore pairing token, AES-256-GCM encrypted under a non-exportable Android Keystore
 * key. Only ciphertext reaches SharedPreferences, in its own file (`ldmux_mesh`), apart from
 * the daemon token in `ldmux`. Uses `javax.crypto` and the platform Keystore: no new
 * dependency.
 *
 * NOT exercised by the JVM unit tests (the Keystore does not exist there). It is verified
 * only by reading; a device run is outstanding. Any failure to decrypt (restored backup,
 * reset keystore, lost key) yields null from [load], the honest "needs pairing" state.
 */
class KeystoreMeshTokenStore(private val context: Context) : MeshTokenStore {
    override fun load(): ByteArray? = try {
        val raw = prefs().getString(KEY_BLOB, null) ?: return null
        val blob = Base64.decode(raw, Base64.NO_WRAP)
        if (blob.size <= IV_BYTES) return null
        val c = Cipher.getInstance(TRANSFORM)
        c.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(TAG_BITS, blob, 0, IV_BYTES))
        c.doFinal(blob, IV_BYTES, blob.size - IV_BYTES)
    } catch (_: Exception) {
        null
    }

    override fun save(token: ByteArray) {
        val c = Cipher.getInstance(TRANSFORM)
        c.init(Cipher.ENCRYPT_MODE, key())
        val blob = c.iv + c.doFinal(token)
        prefs().edit().putString(KEY_BLOB, Base64.encodeToString(blob, Base64.NO_WRAP)).apply()
    }

    override fun clear() {
        prefs().edit().remove(KEY_BLOB).apply()
    }

    private fun prefs() = context.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    private fun key(): SecretKey {
        val ks = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (ks.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val gen = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore")
        gen.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return gen.generateKey()
    }

    private companion object {
        const val FILE = "ldmux_mesh"
        const val KEY_BLOB = "mesh_token_v1"
        const val ALIAS = "ldmux.mesh.token.v1"
        const val TRANSFORM = "AES/GCM/NoPadding"
        const val IV_BYTES = 12
        const val TAG_BITS = 128
    }
}
