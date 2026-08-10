package com.onekaleidoscope.network

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class NetworkEpochTrackerTest {
    @Test
    fun staleCallbacksCannotRollBackNewDefaultNetwork() {
        val tracker = NetworkEpochTracker<String>()
        val first = tracker.available("wifi")
        tracker.capabilities("wifi", usable = true)
        val second = tracker.available("cellular")

        assertNull(tracker.lost("wifi"))
        assertNull(tracker.capabilities("wifi", usable = false))
        val cellular = requireNotNull(tracker.capabilities("cellular", usable = true))
        assertTrue(cellular.usable)
        assertTrue(second.epoch > first.epoch)
        assertEquals(second.epoch, cellular.epoch)
    }

    @Test
    fun losingCurrentNetworkAdvancesEpochAndMarksItUnusable() {
        val tracker = NetworkEpochTracker<String>()
        val available = tracker.available("cellular")
        tracker.capabilities("cellular", usable = true)

        val lost = requireNotNull(tracker.lost("cellular"))

        assertTrue(lost.epoch > available.epoch)
        assertEquals(false, lost.usable)
    }
}
