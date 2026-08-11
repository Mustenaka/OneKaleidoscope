package com.onekaleidoscope.push

import android.util.Log

/** Fixed-vocabulary logging prevents FID, wake payload, endpoint and filesystem disclosure. */
internal object MobileSecurityLog {
    fun record(event: Event) {
        Log.i(TAG, message(event))
    }

    internal fun message(event: Event): String = when (event) {
        Event.FidRegistered -> "push address recorded"
        Event.FidUnregistered -> "push address deletion recorded"
        Event.WakeAccepted -> "wake accepted"
        Event.WakeRejected -> "wake rejected"
        Event.MessagesDeleted -> "push delivery gap detected"
        Event.NetworkChanged -> "default network changed"
        Event.RecoveryScheduled -> "remote recovery scheduled"
    }

    internal enum class Event {
        FidRegistered,
        FidUnregistered,
        WakeAccepted,
        WakeRejected,
        MessagesDeleted,
        NetworkChanged,
        RecoveryScheduled,
    }

    private const val TAG = "KaleidoMobile"
}
