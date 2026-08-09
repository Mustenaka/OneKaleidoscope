package com.onekaleidoscope.platform

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.GeneralSecurityException
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PrivateKey
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import uniffi.kaleido_core.DeviceSigner
import uniffi.kaleido_core.DeviceSignerException

/**
 * P-256 device identity whose private key never leaves AndroidKeyStore.
 *
 * The only exported operations expose the public SPKI or ask Keystore to sign
 * the exact Rust-provided authentication transcript. There is deliberately no
 * private-key getter, encoder, backup or import surface.
 */
class AndroidDeviceSigner(
    private val alias: String = DEVICE_KEY_ALIAS,
) : DeviceSigner {
    private val lock = Any()

    override fun publicKeySpkiDer(): ByteArray =
        try {
            val publicKey = keyPair().public
            val ecPublicKey = publicKey as? ECPublicKey
                ?: throw DeviceSignerException.InvalidPublicKey()
            if (ecPublicKey.params.curve.field.fieldSize != P256_FIELD_BITS ||
                ecPublicKey.params.order.bitLength() != P256_FIELD_BITS
            ) {
                throw DeviceSignerException.InvalidPublicKey()
            }
            publicKey.encoded?.copyOf()
                ?: throw DeviceSignerException.InvalidPublicKey()
        } catch (error: DeviceSignerException) {
            throw error
        } catch (_: GeneralSecurityException) {
            throw DeviceSignerException.KeyUnavailable()
        } catch (_: RuntimeException) {
            throw DeviceSignerException.KeyUnavailable()
        }

    override fun signP256Sha256(transcript: ByteArray): ByteArray {
        if (transcript.isEmpty()) {
            throw DeviceSignerException.SigningFailed()
        }
        return try {
            val signer = Signature.getInstance(SIGNATURE_ALGORITHM)
            signer.initSign(keyPair().private)
            signer.update(transcript)
            val signature = signer.sign()
            if (!StrictEcdsaDer.isCanonicalP256(signature)) {
                throw DeviceSignerException.SigningFailed()
            }
            signature
        } catch (error: DeviceSignerException) {
            throw error
        } catch (_: GeneralSecurityException) {
            throw DeviceSignerException.SigningFailed()
        } catch (_: RuntimeException) {
            throw DeviceSignerException.SigningFailed()
        }
    }

    private fun keyPair(): KeyPair = synchronized(lock) {
        val keyStore = KeyStore.getInstance(ANDROID_KEYSTORE).apply { load(null) }
        val existingPrivate = keyStore.getKey(alias, null) as? PrivateKey
        val existingPublic = keyStore.getCertificate(alias)?.publicKey
        if (existingPrivate != null && existingPublic != null) {
            if (existingPrivate.algorithm != KeyProperties.KEY_ALGORITHM_EC) {
                throw DeviceSignerException.KeyUnavailable()
            }
            return@synchronized KeyPair(existingPublic, existingPrivate)
        }
        if (keyStore.containsAlias(alias)) {
            throw DeviceSignerException.KeyUnavailable()
        }

        val parameters = KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_SIGN)
            .setAlgorithmParameterSpec(ECGenParameterSpec(P256_CURVE))
            .setDigests(KeyProperties.DIGEST_SHA256)
            .setUserAuthenticationRequired(false)
            .build()
        KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, ANDROID_KEYSTORE).run {
            initialize(parameters)
            generateKeyPair()
        }
    }

    companion object {
        const val DEVICE_KEY_ALIAS = "onekaleidoscope.device-auth.p256.v1"
        private const val ANDROID_KEYSTORE = "AndroidKeyStore"
        private const val SIGNATURE_ALGORITHM = "SHA256withECDSA"
        private const val P256_CURVE = "secp256r1"
        private const val P256_FIELD_BITS = 256
    }
}

/** Strict, minimal DER validation before a signature crosses into Rust. */
internal object StrictEcdsaDer {
    fun isCanonicalP256(signature: ByteArray): Boolean {
        if (signature.size < MIN_SIGNATURE_BYTES || unsigned(signature[0]) != SEQUENCE_TAG) {
            return false
        }
        val sequenceLength = unsigned(signature[1])
        if (sequenceLength >= LONG_FORM_LENGTH || sequenceLength != signature.size - 2) {
            return false
        }
        val afterR = integerEnd(signature, 2) ?: return false
        val afterS = integerEnd(signature, afterR) ?: return false
        return afterS == signature.size
    }

    private fun integerEnd(bytes: ByteArray, offset: Int): Int? {
        if (offset > bytes.size - 2 || unsigned(bytes[offset]) != INTEGER_TAG) {
            return null
        }
        val length = unsigned(bytes[offset + 1])
        val start = offset + 2
        val end = start + length
        if (length !in 1..MAX_P256_INTEGER_BYTES || end > bytes.size) {
            return null
        }
        val first = unsigned(bytes[start])
        if (first and SIGN_BIT != 0) {
            return null
        }
        if (length > 1 && first == 0 && unsigned(bytes[start + 1]) and SIGN_BIT == 0) {
            return null
        }
        val magnitudeStart = if (first == 0) start + 1 else start
        if ((magnitudeStart until end).all { bytes[it].toInt() == 0 }) {
            return null
        }
        return end
    }

    private fun unsigned(byte: Byte): Int = byte.toInt() and 0xff

    private const val MIN_SIGNATURE_BYTES = 8
    private const val MAX_P256_INTEGER_BYTES = 33
    private const val LONG_FORM_LENGTH = 0x80
    private const val SIGN_BIT = 0x80
    private const val SEQUENCE_TAG = 0x30
    private const val INTEGER_TAG = 0x02
}
