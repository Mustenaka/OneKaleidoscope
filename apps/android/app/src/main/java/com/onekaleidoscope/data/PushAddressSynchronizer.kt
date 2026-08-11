package com.onekaleidoscope.data

import com.onekaleidoscope.push.PushAddressState

internal interface PushAddressCorePort {
    fun deleteAddress(): Boolean
    fun replaceAddress(fid: String, observedAtMs: Long, expiresAtMs: Long): Boolean
    fun flushOutbox(): Boolean
}

/** Orders Android's latest durable FID state over Rust's durable remote-control outbox. */
internal object PushAddressSynchronizer {
    fun synchronize(state: PushAddressState, port: PushAddressCorePort): PushSyncOutcome {
        var deletionsAcknowledged = false
        if (state.deletionTombstones.isNotEmpty()) {
            if (!port.deleteAddress()) return PushSyncOutcome.Pending
            deletionsAcknowledged = true
        }

        val fid = state.currentFid
        if (fid != null) {
            val observedAtMs = state.currentObservedAtMs
                ?: return PushSyncOutcome(deletionsAcknowledged, complete = false)
            val expiresAtMs = state.currentExpiresAtMs
                ?: return PushSyncOutcome(deletionsAcknowledged, complete = false)
            if (!port.replaceAddress(fid, observedAtMs, expiresAtMs)) {
                return PushSyncOutcome(deletionsAcknowledged, complete = false)
            }
        }

        return PushSyncOutcome(deletionsAcknowledged, complete = port.flushOutbox())
    }
}

internal data class PushSyncOutcome(
    val deletionsAcknowledged: Boolean,
    val complete: Boolean,
) {
    companion object {
        val Pending = PushSyncOutcome(deletionsAcknowledged = false, complete = false)
        val Deferred = PushSyncOutcome(deletionsAcknowledged = false, complete = true)
    }
}
