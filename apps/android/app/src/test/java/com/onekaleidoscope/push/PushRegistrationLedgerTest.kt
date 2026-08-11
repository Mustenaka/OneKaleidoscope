package com.onekaleidoscope.push

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class PushRegistrationLedgerTest {
    @Test
    fun rotationKeepsNewFidAndTombstonesOldFid() {
        val empty = PushAddressState(null, emptySet(), 0)
        val first = PushRegistrationLedger.registered(empty, "fid-first", 1_000)
        val rotated = PushRegistrationLedger.registered(first, "fid-second", 2_000)
        val lateOldDelete = PushRegistrationLedger.unregistered(rotated, "fid-first")

        assertEquals("fid-second", lateOldDelete.currentFid)
        assertEquals(setOf("fid-first"), lateOldDelete.deletionTombstones)
        assertEquals(2, lateOldDelete.revision)
        assertEquals(2_000L, lateOldDelete.currentObservedAtMs)
        assertEquals(2_000L + PushRegistrationLedger.ADDRESS_TTL_MS, lateOldDelete.currentExpiresAtMs)
    }

    @Test
    fun unregisteringCurrentFidCreatesDurableDeleteTombstone() {
        val registered = PushAddressState("fid-current", emptySet(), 8)

        val deleted = PushRegistrationLedger.unregistered(registered, "fid-current")

        assertNull(deleted.currentFid)
        assertTrue("fid-current" in deleted.deletionTombstones)
        assertEquals(9, deleted.revision)
    }

    @Test
    fun invalidFidFailsLoudInsteadOfBeingDropped() {
        assertThrows(IllegalArgumentException::class.java) {
            PushRegistrationLedger.registered(PushAddressState(null, emptySet(), 0), "\n")
        }
    }

    @Test
    fun startupObservationRefreshesThirtyDayExpiry() {
        val registered = PushRegistrationLedger.registered(
            PushAddressState(null, emptySet(), 0),
            "fid-current",
            10_000,
        )

        val refreshed = PushRegistrationLedger.registered(registered, "fid-current", 20_000)

        assertEquals(20_000L, refreshed.currentObservedAtMs)
        assertEquals(20_000L + PushRegistrationLedger.ADDRESS_TTL_MS, refreshed.currentExpiresAtMs)
        assertEquals(2, refreshed.revision)
    }

    @Test
    fun lateUnregisterForAnOldFidCannotDeleteTheCurrentAddress() {
        val current = PushAddressState(
            currentFid = "fid-current",
            deletionTombstones = emptySet(),
            revision = 7,
            currentObservedAtMs = 1_000,
            currentExpiresAtMs = 2_000,
        )

        val unchanged = PushRegistrationLedger.unregistered(current, "fid-retired")

        assertEquals("fid-current", unchanged.currentFid)
        assertTrue(unchanged.deletionTombstones.isEmpty())
        assertEquals(7, unchanged.revision)
    }
}
