package com.onekaleidoscope.ui

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class UiModelsTest {
    @Test
    fun disabledActionRequiresConcreteUserVisibleReason() {
        val reason = "Runtime 尚未证明 TurnPrompt"

        val availability = ActionAvailability.disabled(reason)

        assertEquals(false, availability.enabled)
        assertEquals(reason, availability.disabledReason)
        assertThrows(IllegalArgumentException::class.java) {
            ActionAvailability(enabled = false, disabledReason = "  ")
        }
    }

    @Test
    fun pendingQueueStateIsDistinctFromBothDeliveredOutcomes() {
        val delivered = setOf(QueueStateUi.DeliveredNewTurn, QueueStateUi.DeliveredSteer)

        assertEquals(false, QueueStateUi.Pending in delivered)
        assertEquals(false, QueueStateUi.Submitting in delivered)
    }
}
