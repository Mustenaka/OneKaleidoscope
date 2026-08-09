package com.onekaleidoscope.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PendingActionTrackerTest {
    @Test
    fun concurrentDuplicateIsRejectedWithoutMintingAnotherIdempotencyKey() {
        val tracker = PendingActionTracker()
        val signature = promptSignature("same body")
        var createdKeys = 0

        val first = tracker.begin(PendingActionSlot.Prompt, signature) { "key-${++createdKeys}" }
        val duplicate = tracker.begin(PendingActionSlot.Prompt, signature) { "key-${++createdKeys}" }

        assertEquals("key-1", first?.idempotencyKey)
        assertNull(duplicate)
        assertEquals(1, createdKeys)
        assertTrue(tracker.isInFlight(PendingActionSlot.Prompt))
    }

    @Test
    fun uncertainRetryReusesKeyButDefiniteCompletionClearsIt() {
        val tracker = PendingActionTracker()
        val signature = promptSignature("retry body")
        var createdKeys = 0

        val first = requireNotNull(
            tracker.begin(PendingActionSlot.Prompt, signature) { "key-${++createdKeys}" },
        )
        tracker.complete(PendingActionSlot.Prompt, CommandCompletion.Uncertain)
        assertFalse(tracker.isInFlight(PendingActionSlot.Prompt))

        val retry = requireNotNull(
            tracker.begin(PendingActionSlot.Prompt, signature) { "key-${++createdKeys}" },
        )
        assertEquals(first.idempotencyKey, retry.idempotencyKey)
        assertEquals(1, createdKeys)

        tracker.complete(PendingActionSlot.Prompt, CommandCompletion.DefiniteSuccess)
        assertNull(tracker.pending(PendingActionSlot.Prompt))
        val next = requireNotNull(
            tracker.begin(PendingActionSlot.Prompt, signature) { "key-${++createdKeys}" },
        )
        assertEquals("key-2", next.idempotencyKey)
    }

    @Test
    fun changedPayloadAfterUncertainResultGetsANewKey() {
        val tracker = PendingActionTracker()
        var createdKeys = 0
        val first = requireNotNull(
            tracker.begin(PendingActionSlot.EnqueueNewTurn, enqueueSignature("first")) {
                "key-${++createdKeys}"
            },
        )
        tracker.complete(PendingActionSlot.EnqueueNewTurn, CommandCompletion.Uncertain)

        val changed = requireNotNull(
            tracker.begin(PendingActionSlot.EnqueueNewTurn, enqueueSignature("second")) {
                "key-${++createdKeys}"
            },
        )

        assertEquals("key-1", first.idempotencyKey)
        assertEquals("key-2", changed.idempotencyKey)
        assertEquals(2, createdKeys)
    }

    @Test
    fun attentionSlotsAreIsolatedPerAttentionId() {
        val tracker = PendingActionTracker()
        val firstSlot = PendingActionSlot.Attention("attention-1")
        val secondSlot = PendingActionSlot.Attention("attention-2")

        assertEquals(
            "key-1",
            tracker.begin(firstSlot, attentionSignature("attention-1")) { "key-1" }?.idempotencyKey,
        )
        assertEquals(
            "key-2",
            tracker.begin(secondSlot, attentionSignature("attention-2")) { "key-2" }?.idempotencyKey,
        )
        assertTrue(tracker.isInFlight(firstSlot))
        assertTrue(tracker.isInFlight(secondSlot))

        tracker.complete(firstSlot, CommandCompletion.DefiniteRejection)
        assertFalse(tracker.isInFlight(firstSlot))
        assertNull(tracker.pending(firstSlot))
        assertTrue(tracker.isInFlight(secondSlot))
    }

    @Test
    fun answeredAttentionRemainsDisabledUntilAProjectionRemovesIt() {
        val tracker = PendingActionTracker()
        val slot = PendingActionSlot.Attention("attention-1")
        val signature = attentionSignature("attention-1")
        var createdKeys = 0

        assertEquals(
            "key-1",
            tracker.begin(slot, signature) { "key-${++createdKeys}" }?.idempotencyKey,
        )
        tracker.complete(
            slot,
            CommandCompletion.DefiniteSuccess,
            retainDefiniteSuccess = true,
        )
        assertTrue(tracker.isResolved(slot))
        assertNull(tracker.begin(slot, signature) { "key-${++createdKeys}" })

        tracker.retainResolvedAttention(setOf("attention-1"))
        assertTrue(tracker.isResolved(slot))
        tracker.retainResolvedAttention(emptySet())
        assertFalse(tracker.isResolved(slot))
        assertEquals(
            "key-2",
            tracker.begin(slot, signature) { "key-${++createdKeys}" }?.idempotencyKey,
        )
    }

    private fun promptSignature(text: String) = PendingActionSignature(
        kind = PendingActionKind.Prompt,
        targetId = "session-1",
        text = text,
        optionId = null,
    )

    private fun enqueueSignature(text: String) = PendingActionSignature(
        kind = PendingActionKind.EnqueueNewTurn,
        targetId = "session-1",
        text = text,
        optionId = null,
    )

    private fun attentionSignature(attentionId: String) = PendingActionSignature(
        kind = PendingActionKind.Attention,
        targetId = attentionId,
        text = "",
        optionId = "approve",
    )
}
