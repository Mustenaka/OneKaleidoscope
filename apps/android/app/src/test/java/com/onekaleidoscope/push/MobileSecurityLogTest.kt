package com.onekaleidoscope.push

import org.junit.Assert.assertFalse
import org.junit.Test

class MobileSecurityLogTest {
    @Test
    fun fixedMessagesCannotContainSensitiveRuntimeValues() {
        val forbidden = listOf(
            "sensitive-fid",
            "abcdefghijklmnopqrstuv",
            "https://relay.example.invalid",
            "C:\\Users\\person\\private",
        )

        MobileSecurityLog.Event.entries.forEach { event ->
            val message = MobileSecurityLog.message(event)
            forbidden.forEach { secret -> assertFalse(message.contains(secret)) }
            assertFalse(message.contains("="))
            assertFalse(message.contains("/"))
        }
    }

    @Test
    fun sensitiveValueDebugStringsAreRedacted() {
        val validation = WakePayload.validate(
            mapOf(
                "v" to "1",
                "kind" to "wake",
                "route" to "abcdefghijklmnopqrstuv",
                "wake" to "ABCDEFGHIJKLMNOPQRSTUV",
            ),
            hasNotification = false,
        )
        val address = PushAddressState(
            currentFid = "sensitive-fid",
            deletionTombstones = setOf("old-sensitive-fid"),
            revision = 2,
        )

        assertFalse(validation.toString().contains("abcdefghijklmnopqrstuv"))
        assertFalse(validation.toString().contains("ABCDEFGHIJKLMNOPQRSTUV"))
        assertFalse(address.toString().contains("sensitive-fid"))
        assertFalse(address.toString().contains("old-sensitive-fid"))
    }
}
