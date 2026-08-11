package com.onekaleidoscope.data

import com.onekaleidoscope.push.PushAddressState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PushAddressSynchronizerTest {
    @Test
    fun rotationDeletesOldAddressBeforeReplacingWithCurrentFid() {
        val port = RecordingPort()
        val state = PushAddressState(
            currentFid = "fid-new",
            deletionTombstones = setOf("fid-old"),
            revision = 2,
            currentObservedAtMs = 1_000,
            currentExpiresAtMs = 2_000,
        )

        val outcome = PushAddressSynchronizer.synchronize(state, port)

        assertEquals(listOf("delete", "replace:fid-new:1000:2000", "flush"), port.calls)
        assertTrue(outcome.deletionsAcknowledged)
        assertTrue(outcome.complete)
    }

    @Test
    fun failedDeleteDoesNotOverwriteItWithAReplacement() {
        val port = RecordingPort(deleteSucceeds = false)
        val state = PushAddressState(
            currentFid = "fid-new",
            deletionTombstones = setOf("fid-old"),
            revision = 2,
            currentObservedAtMs = 1_000,
            currentExpiresAtMs = 2_000,
        )

        val outcome = PushAddressSynchronizer.synchronize(state, port)

        assertEquals(listOf("delete"), port.calls)
        assertFalse(outcome.deletionsAcknowledged)
        assertFalse(outcome.complete)
    }

    @Test
    fun acknowledgedDeleteSurvivesALaterReplacementFailure() {
        val port = RecordingPort(replaceSucceeds = false)
        val state = PushAddressState(
            currentFid = "fid-new",
            deletionTombstones = setOf("fid-old"),
            revision = 2,
            currentObservedAtMs = 1_000,
            currentExpiresAtMs = 2_000,
        )

        val outcome = PushAddressSynchronizer.synchronize(state, port)

        assertEquals(listOf("delete", "replace:fid-new:1000:2000"), port.calls)
        assertTrue(outcome.deletionsAcknowledged)
        assertFalse(outcome.complete)
    }

    private class RecordingPort(
        private val deleteSucceeds: Boolean = true,
        private val replaceSucceeds: Boolean = true,
    ) : PushAddressCorePort {
        val calls = mutableListOf<String>()

        override fun deleteAddress(): Boolean {
            calls += "delete"
            return deleteSucceeds
        }

        override fun replaceAddress(fid: String, observedAtMs: Long, expiresAtMs: Long): Boolean {
            calls += "replace:$fid:$observedAtMs:$expiresAtMs"
            return replaceSucceeds
        }

        override fun flushOutbox(): Boolean {
            calls += "flush"
            return true
        }
    }
}
