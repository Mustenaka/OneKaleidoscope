package com.onekaleidoscope.platform

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class StrictEcdsaDerTest {
    @Test
    fun acceptsMinimalPositiveP256Integers() {
        val signature = bytes(0x30, 0x07, 0x02, 0x01, 0x01, 0x02, 0x02, 0x00, 0x80)

        assertTrue(StrictEcdsaDer.isCanonicalP256(signature))
    }

    @Test
    fun rejectsTrailingNegativeZeroAndRedundantEncodings() {
        assertFalse(
            StrictEcdsaDer.isCanonicalP256(
                bytes(0x30, 0x08, 0x02, 0x01, 0x01, 0x02, 0x02, 0x00, 0x80, 0x00),
            ),
        )
        assertFalse(
            StrictEcdsaDer.isCanonicalP256(
                bytes(0x30, 0x06, 0x02, 0x01, 0x80, 0x02, 0x01, 0x01),
            ),
        )
        assertFalse(
            StrictEcdsaDer.isCanonicalP256(
                bytes(0x30, 0x06, 0x02, 0x01, 0x00, 0x02, 0x01, 0x01),
            ),
        )
        assertFalse(
            StrictEcdsaDer.isCanonicalP256(
                bytes(0x30, 0x07, 0x02, 0x02, 0x00, 0x01, 0x02, 0x01, 0x01),
            ),
        )
    }

    private fun bytes(vararg values: Int): ByteArray =
        ByteArray(values.size) { index -> values[index].toByte() }
}
