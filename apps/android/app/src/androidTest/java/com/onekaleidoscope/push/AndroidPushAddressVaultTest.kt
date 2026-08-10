package com.onekaleidoscope.push

import android.content.Context
import android.util.Base64
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.io.File
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class AndroidPushAddressVaultTest {
    private val context: Context
        get() = InstrumentationRegistry.getInstrumentation().targetContext

    @After
    fun clearVault() {
        context.deleteSharedPreferences(TEST_PREFERENCES_FILE)
    }

    @Test
    fun fidRotationAndDeleteTombstonesAreEncryptedAtRest() {
        val firstCallbackVault = AndroidPushAddressVault(context, TEST_PREFERENCES_FILE)
        val workerVault = AndroidPushAddressVault(context, TEST_PREFERENCES_FILE)
        firstCallbackVault.recordRegistered(FIRST_FID, 1_000)
        workerVault.recordRegistered(SECOND_FID, 2_000)
        firstCallbackVault.recordUnregistered(FIRST_FID)
        val rotated = workerVault.load()
        val deleted = firstCallbackVault.recordUnregistered(SECOND_FID)

        assertEquals(SECOND_FID, rotated.currentFid)
        assertTrue(FIRST_FID in rotated.deletionTombstones)
        assertEquals(2_000L, rotated.currentObservedAtMs)
        assertEquals(2_000L + PushRegistrationLedger.ADDRESS_TTL_MS, rotated.currentExpiresAtMs)
        assertNull(deleted.currentFid)
        assertEquals(setOf(FIRST_FID, SECOND_FID), deleted.deletionTombstones)

        val persisted = File(
            File(context.applicationInfo.dataDir, "shared_prefs"),
            "$TEST_PREFERENCES_FILE.xml",
        ).readText()
        listOf(FIRST_FID, SECOND_FID).forEach { fid ->
            assertFalse(persisted.contains(fid))
            assertFalse(persisted.contains(Base64.encodeToString(fid.encodeToByteArray(), Base64.NO_WRAP)))
        }
    }

    private companion object {
        const val FIRST_FID = "opaque-firebase-installation-one"
        const val SECOND_FID = "opaque-firebase-installation-two"
        const val TEST_PREFERENCES_FILE = "onekaleidoscope-push-address-instrumentation"
    }
}
