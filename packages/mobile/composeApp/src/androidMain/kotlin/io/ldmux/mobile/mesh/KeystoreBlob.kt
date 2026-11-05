package io.ldmux.mobile.mesh

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
 * One secret blob, AES-256-GCM encrypted under a non-exportable Android Keystore key, with only
 * ciphertext in the `ldmux_mesh` SharedPreferences file. The mesh token and the session keys each
 * use one, under different preference keys and the same Keystore key. Uses `javax.crypto` and the
 * platform Keystore: no dependency.
 *
 * NOT exercised by the JVM unit tests (the Keystore does not exist there); a device run is
 * outstanding. Any failure to decrypt (restored backup, reset keystore, lost key) is null from
 * [load], the honest "needs pairing" state.
 */
internal class KeystoreBlob(
    private val context: Context,
    private val prefsKey: String,
    private val file: String = FILE,
) {
    fun load(): ByteArray? = try {
        val raw = prefs().getString(prefsKey, null) ?: return null
        val blob = Base64.decode(raw, Base64.NO_WRAP)
        if (blob.size <= IV_BYTES) return null
        val c = Cipher.getInstance(TRANSFORM)
        c.init(Cipher.DECRYPT_MODE, key(), GCMParameterSpec(TAG_BITS, blob, 0, IV_BYTES))
        c.doFinal(blob, IV_BYTES, blob.size - IV_BYTES)
    } catch (_: Exception) {
        null
    }

    fun save(secret: ByteArray) {
        val c = Cipher.getInstance(TRANSFORM)
        c.init(Cipher.ENCRYPT_MODE, key())
        val blob = c.iv + c.doFinal(secret)
        prefs().edit().putString(prefsKey, Base64.encodeToString(blob, Base64.NO_WRAP)).apply()
    }

    fun clear() {
        prefs().edit().remove(prefsKey).apply()
    }

    private fun prefs() = context.getSharedPreferences(file, Context.MODE_PRIVATE)

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
        // Unchanged from the token store this was extracted from, so an existing pairing survives.
        const val ALIAS = "ldmux.mesh.token.v1"
        const val TRANSFORM = "AES/GCM/NoPadding"
        const val IV_BYTES = 12
        const val TAG_BITS = 128
    }
}
