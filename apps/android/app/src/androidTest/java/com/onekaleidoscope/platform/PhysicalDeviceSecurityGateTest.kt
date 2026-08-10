package com.onekaleidoscope.platform

import android.os.Build
import android.security.keystore.KeyInfo
import android.security.keystore.KeyProperties
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import java.security.KeyFactory
import java.security.KeyStore
import java.security.PrivateKey
import org.junit.After
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

/** Explicitly selected only by the externally orchestrated physical-device gate. */
@RunWith(AndroidJUnit4::class)
class PhysicalDeviceSecurityGateTest {
    @After
    fun removeGateKey() {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        if (keyStore.containsAlias(PHYSICAL_GATE_ALIAS)) {
            keyStore.deleteEntry(PHYSICAL_GATE_ALIAS)
        }
    }

    @Test
    fun deviceIdentityIsNonExportableAndHardwareBacked() {
        val required = InstrumentationRegistry.getArguments()
            .getString(ARG_REQUIRE_HARDWARE_BACKED)
            ?.toBooleanStrictOrNull() == true
        assumeTrue("selected only by the physical arm64 gate", required)

        val signer = AndroidDeviceSigner(PHYSICAL_GATE_ALIAS)
        assertTrue(signer.publicKeySpkiDer().isNotEmpty())
        assertTrue(StrictEcdsaDer.isCanonicalP256(signer.signP256Sha256("physical gate".encodeToByteArray())))

        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        val privateKey = keyStore.getKey(PHYSICAL_GATE_ALIAS, null) as PrivateKey
        assertNull("device private key became exportable", privateKey.encoded)
        val keyInfo = KeyFactory.getInstance(privateKey.algorithm, ANDROID_KEYSTORE)
            .getKeySpec(privateKey, KeyInfo::class.java)
        val hardwareBacked = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            keyInfo.securityLevel == KeyProperties.SECURITY_LEVEL_TRUSTED_ENVIRONMENT ||
                keyInfo.securityLevel == KeyProperties.SECURITY_LEVEL_STRONGBOX
        } else {
            @Suppress("DEPRECATION")
            keyInfo.isInsideSecureHardware
        }
        assertTrue("AndroidKeyStore P-256 identity is not hardware-backed", hardwareBacked)
    }

    companion object {
        private const val ARG_REQUIRE_HARDWARE_BACKED = "requireHardwareBacked"
        private const val PHYSICAL_GATE_ALIAS = "onekaleidoscope.physical-gate.p256"
        private const val ANDROID_KEYSTORE = "AndroidKeyStore"
    }
}
