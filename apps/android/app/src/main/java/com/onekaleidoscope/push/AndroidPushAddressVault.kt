package com.onekaleidoscope.push

import android.annotation.SuppressLint
import android.content.Context
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey

/** Encrypted, fail-loud persistence for opaque FCM installation identifiers. */
internal class AndroidPushAddressVault(
    context: Context,
    private val preferencesFile: String = PREFERENCES_FILE,
) {
    private val applicationContext = context.applicationContext
    private val preferences by lazy(LazyThreadSafetyMode.SYNCHRONIZED) {
        val masterKey = MasterKey.Builder(applicationContext, MASTER_KEY_ALIAS)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            applicationContext,
            preferencesFile,
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    fun load(): PushAddressState = synchronized(VAULT_LOCK) { loadLocked() }

    private fun loadLocked(): PushAddressState = try {
        val current = preferences.getString(CURRENT_FID_KEY, null)
        current?.let(PushRegistrationLedger::requireValidFid)
        val tombstones = preferences.getStringSet(DELETION_TOMBSTONES_KEY, emptySet())
            ?.toSet()
            ?: throw PushAddressVaultException.Corrupt()
        if (tombstones.size > PushRegistrationLedger.MAX_DELETION_TOMBSTONES) {
            throw PushAddressVaultException.Corrupt()
        }
        tombstones.forEach(PushRegistrationLedger::requireValidFid)
        if (current != null && current in tombstones) throw PushAddressVaultException.Corrupt()
        val revision = preferences.getLong(REVISION_KEY, 0)
        if (revision < 0) throw PushAddressVaultException.Corrupt()
        val observedAtMs = preferences.getLong(CURRENT_OBSERVED_AT_KEY, MISSING_TIME).takeIf { it >= 0 }
        val expiresAtMs = preferences.getLong(CURRENT_EXPIRES_AT_KEY, MISSING_TIME).takeIf { it >= 0 }
        if ((current == null) != (observedAtMs == null) ||
            (current == null) != (expiresAtMs == null) ||
            (observedAtMs != null && expiresAtMs != null && expiresAtMs <= observedAtMs)
        ) {
            throw PushAddressVaultException.Corrupt()
        }
        PushAddressState(current, tombstones, revision, observedAtMs, expiresAtMs)
    } catch (error: PushAddressVaultException) {
        throw error
    } catch (_: IllegalArgumentException) {
        throw PushAddressVaultException.Corrupt()
    } catch (_: RuntimeException) {
        throw PushAddressVaultException.Unavailable()
    }

    fun recordRegistered(
        fid: String,
        observedAtMs: Long = System.currentTimeMillis(),
    ): PushAddressState = synchronized(VAULT_LOCK) {
        persist(PushRegistrationLedger.registered(loadLocked(), fid, observedAtMs))
    }

    fun recordUnregistered(fid: String): PushAddressState = synchronized(VAULT_LOCK) {
        persist(PushRegistrationLedger.unregistered(loadLocked(), fid))
    }

    fun acknowledgeDeletion(fid: String): PushAddressState = synchronized(VAULT_LOCK) {
        persist(PushRegistrationLedger.deletionAcknowledged(loadLocked(), fid))
    }

    fun refreshCurrent(observedAtMs: Long = System.currentTimeMillis()): PushAddressState =
        synchronized(VAULT_LOCK) {
            val state = loadLocked()
            val current = state.currentFid ?: return@synchronized state
            persist(PushRegistrationLedger.registered(state, current, observedAtMs))
        }

    @SuppressLint("UseKtx")
    private fun persist(state: PushAddressState): PushAddressState = try {
        val editor = preferences.edit()
            .putStringSet(DELETION_TOMBSTONES_KEY, state.deletionTombstones.toSet())
            .putLong(REVISION_KEY, state.revision)
        if (state.currentFid == null) {
            editor.remove(CURRENT_FID_KEY)
                .remove(CURRENT_OBSERVED_AT_KEY)
                .remove(CURRENT_EXPIRES_AT_KEY)
        } else {
            editor.putString(CURRENT_FID_KEY, state.currentFid)
                .putLong(CURRENT_OBSERVED_AT_KEY, checkNotNull(state.currentObservedAtMs))
                .putLong(CURRENT_EXPIRES_AT_KEY, checkNotNull(state.currentExpiresAtMs))
        }
        if (!editor.commit()) throw PushAddressVaultException.Unavailable()
        state
    } catch (error: PushAddressVaultException) {
        throw error
    } catch (_: RuntimeException) {
        throw PushAddressVaultException.Unavailable()
    }

    companion object {
        internal const val PREFERENCES_FILE = "onekaleidoscope-push-address-v1"
        private const val MASTER_KEY_ALIAS = "onekaleidoscope.push-address.master-key.v1"
        private const val CURRENT_FID_KEY = "current-fid"
        private const val DELETION_TOMBSTONES_KEY = "deletion-tombstones"
        private const val REVISION_KEY = "revision"
        private const val CURRENT_OBSERVED_AT_KEY = "current-observed-at-ms"
        private const val CURRENT_EXPIRES_AT_KEY = "current-expires-at-ms"
        private const val MISSING_TIME = -1L
        private val VAULT_LOCK = Any()
    }
}

internal sealed class PushAddressVaultException(message: String) : IllegalStateException(message) {
    class Corrupt : PushAddressVaultException("encrypted push address state is corrupt")
    class Unavailable : PushAddressVaultException("encrypted push address storage is unavailable")
}
