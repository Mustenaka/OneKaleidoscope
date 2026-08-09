package com.onekaleidoscope.platform

import android.annotation.SuppressLint
import android.content.Context
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import uniffi.kaleido_core.SecureCredentialVault
import uniffi.kaleido_core.SecureCredentialVaultException

/**
 * Encrypted persistence for Rust-owned opaque paired-host bytes.
 *
 * Kotlin never decodes endpoint, host pin or DeviceId. Base64 is only the
 * reversible container required by SharedPreferences' encrypted String API.
 */
class AndroidSecureCredentialVault(context: Context) : SecureCredentialVault {
    private val applicationContext = context.applicationContext

    private val preferences by lazy(LazyThreadSafetyMode.SYNCHRONIZED) {
        val masterKey = MasterKey.Builder(applicationContext, MASTER_KEY_ALIAS)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            applicationContext,
            PREFERENCES_FILE,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    override fun loadPairedHost(): ByteArray? =
        try {
            val encoded = preferences.getString(PAIRED_HOST_KEY, null) ?: return null
            val decoded = Base64.decode(encoded, Base64.NO_WRAP)
            if (Base64.encodeToString(decoded, Base64.NO_WRAP) != encoded) {
                throw SecureCredentialVaultException.Corrupt()
            }
            decoded
        } catch (error: SecureCredentialVaultException) {
            throw error
        } catch (_: IllegalArgumentException) {
            throw SecureCredentialVaultException.Corrupt()
        } catch (_: RuntimeException) {
            throw SecureCredentialVaultException.Unavailable()
        }

    @SuppressLint("UseKtx") // The boolean result of synchronous commit is a security boundary.
    override fun storePairedHost(credential: ByteArray) {
        if (credential.isEmpty()) {
            throw SecureCredentialVaultException.Corrupt()
        }
        try {
            val encoded = Base64.encodeToString(credential, Base64.NO_WRAP)
            if (!preferences.edit().putString(PAIRED_HOST_KEY, encoded).commit()) {
                throw SecureCredentialVaultException.Unavailable()
            }
        } catch (error: SecureCredentialVaultException) {
            throw error
        } catch (_: RuntimeException) {
            throw SecureCredentialVaultException.Unavailable()
        }
    }

    companion object {
        private const val MASTER_KEY_ALIAS = "onekaleidoscope.master-key.v1"
        private const val PREFERENCES_FILE = "onekaleidoscope-secure-credentials-v1"
        private const val PAIRED_HOST_KEY = "rust-paired-host-envelope"
    }
}
