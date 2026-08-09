package com.onekaleidoscope.data

import com.onekaleidoscope.ui.DataFreshness
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ProjectionCursorHighWaterTest {
    @Test
    fun newerCacheRejectsStaleQueuedLiveCallbackButAllowsEqualLiveConfirmation() {
        val highWater = ProjectionCursorHighWater<String>()

        assertTrue(highWater.acceptLive("queue/session-1", 10uL))
        assertTrue(highWater.acceptCached("queue/session-1", 20uL))
        assertFalse(highWater.acceptLive("queue/session-1", 11uL))
        assertTrue(highWater.acceptLive("queue/session-1", 20uL))
    }

    @Test
    fun cursorHighWaterIsIsolatedPerProjectionKey() {
        val highWater = ProjectionCursorHighWater<String>()

        assertTrue(highWater.acceptCached("queue/session-1", 20uL))
        assertTrue(highWater.acceptCached("capability/runtime-1", 2uL))
        assertFalse(highWater.acceptLive("queue/session-1", 19uL))
        assertTrue(highWater.acceptLive("capability/runtime-1", 2uL))
    }

    @Test
    fun synchronizedCacheConfirmationAcceptsAnEqualOfflineCacheCursor() {
        val highWater = ProjectionCursorHighWater<String>()

        assertTrue(highWater.acceptCached("transcript/session-1", 42uL))
        assertEquals(
            DataFreshness.Live,
            highWater.synchronizedFreshness("transcript/session-1", 42uL),
        )
        assertNull(highWater.synchronizedFreshness("transcript/session-1", 41uL))
    }
}
