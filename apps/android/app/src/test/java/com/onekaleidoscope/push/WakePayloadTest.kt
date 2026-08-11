package com.onekaleidoscope.push

import org.junit.Assert.assertEquals
import org.junit.Test

class WakePayloadTest {
    @Test
    fun acceptsOnlyCanonicalDataOnlyWake() {
        val result = WakePayload.validate(validData(), hasNotification = false)

        assertEquals(WakePayloadValidation.Accepted, result)
    }

    @Test
    fun rejectsUnknownFieldAndNotificationPayload() {
        val withUnknown = validData() + ("state" to "running")

        assertEquals(
            WakePayloadRejection.Keys,
            (WakePayload.validate(withUnknown, false) as WakePayloadValidation.Rejected).reason,
        )
        assertEquals(
            WakePayloadRejection.Notification,
            (WakePayload.validate(validData(), true) as WakePayloadValidation.Rejected).reason,
        )
    }

    @Test
    fun rejectsOverlongAndNonCanonicalOpaqueValues() {
        val overlong = validData() + ("wake" to "A".repeat(257))
        val padded = validData() + ("route" to "$ROUTE=")

        assertEquals(
            WakePayloadRejection.OpaqueId,
            (WakePayload.validate(overlong, false) as WakePayloadValidation.Rejected).reason,
        )
        assertEquals(
            WakePayloadRejection.OpaqueId,
            (WakePayload.validate(padded, false) as WakePayloadValidation.Rejected).reason,
        )
    }

    private fun validData() = mapOf(
        "v" to "1",
        "kind" to "wake",
        "route" to ROUTE,
        "wake" to WAKE,
    )

    private companion object {
        const val ROUTE = "abcdefghijklmnopqrstuv"
        const val WAKE = "ABCDEFGHIJKLMNOPQRSTUV"
    }
}
