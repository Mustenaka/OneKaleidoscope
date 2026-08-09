package com.onekaleidoscope.platform

import android.content.Context
import android.util.Base64
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import java.security.KeyStore
import org.junit.After
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AndroidSecurityBridgeTest {
    private val context: Context
        get() = InstrumentationRegistry.getInstrumentation().targetContext

    @After
    fun removeTestKey() {
        val keyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        if (keyStore.containsAlias(TEST_KEY_ALIAS)) {
            keyStore.deleteEntry(TEST_KEY_ALIAS)
        }
    }

    @Test
    fun signerCreatesNonExportableKeyAndStrictDerSignature() {
        val signer = AndroidDeviceSigner(TEST_KEY_ALIAS)

        assertTrue(signer.publicKeySpkiDer().isNotEmpty())
        val signature = signer.signP256Sha256("bound transcript".encodeToByteArray())
        assertTrue(StrictEcdsaDer.isCanonicalP256(signature))

        val keyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        assertNull(keyStore.getKey(TEST_KEY_ALIAS, null).encoded)
    }

    @Test
    fun vaultRoundTripsOpaqueBytesWithoutPlaintextPreferences() {
        val vault = AndroidSecureCredentialVault(context)
        val originalCredential = vault.loadPairedHost()
        val credential = "unique-sensitive-host-pin-and-endpoint".encodeToByteArray()

        try {
            vault.storePairedHost(credential)

            assertArrayEquals(credential, vault.loadPairedHost())
            val persistedXml = File(
                File(context.applicationInfo.dataDir, "shared_prefs"),
                "$SECURE_PREFERENCES_FILE.xml",
            ).readText()
            assertFalse(persistedXml.contains(credential.decodeToString()))
            assertFalse(persistedXml.contains(Base64.encodeToString(credential, Base64.NO_WRAP)))
        } finally {
            if (originalCredential == null) {
                context.deleteSharedPreferences(SECURE_PREFERENCES_FILE)
            } else {
                vault.storePairedHost(originalCredential)
            }
        }
    }

    @Test
    fun projectionCacheIsPhysicallyUnderNoBackupStorage() {
        val noBackup = context.noBackupFilesDir.canonicalFile
        val cache = AndroidCoreStorage.projectionCacheDirectory(context).canonicalFile

        assertTrue(cache.path.startsWith(noBackup.path + File.separator))
        assertTrue(cache.isDirectory)
    }

    companion object {
        private const val TEST_KEY_ALIAS = "onekaleidoscope.instrumentation.p256"
        private const val SECURE_PREFERENCES_FILE = "onekaleidoscope-secure-credentials-v1"
    }
}
