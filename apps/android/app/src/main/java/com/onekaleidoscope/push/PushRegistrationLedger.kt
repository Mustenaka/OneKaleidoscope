package com.onekaleidoscope.push

internal class PushAddressState(
    val currentFid: String?,
    val deletionTombstones: Set<String>,
    val revision: Long,
    val currentObservedAtMs: Long? = null,
    val currentExpiresAtMs: Long? = null,
) {
    override fun toString(): String = "PushAddressState([redacted], revision=$revision)"
}

/** Pure transition logic for FID rotation; persistence is supplied by the Android vault. */
internal object PushRegistrationLedger {
    fun registered(
        state: PushAddressState,
        fid: String,
        observedAtMs: Long = System.currentTimeMillis(),
    ): PushAddressState {
        requireValidFid(fid)
        require(observedAtMs >= 0) { "invalid FCM observation time" }
        val expiresAtMs = Math.addExact(observedAtMs, ADDRESS_TTL_MS)
        val tombstones = state.deletionTombstones.toMutableSet()
        state.currentFid?.takeIf { it != fid }?.let(tombstones::add)
        tombstones.remove(fid)
        require(tombstones.size <= MAX_DELETION_TOMBSTONES) {
            "too many pending FCM deletion tombstones"
        }
        return PushAddressState(
            currentFid = fid,
            deletionTombstones = tombstones,
            revision = state.revision + 1,
            currentObservedAtMs = observedAtMs,
            currentExpiresAtMs = expiresAtMs,
        )
    }

    fun unregistered(state: PushAddressState, fid: String): PushAddressState {
        requireValidFid(fid)
        if (state.currentFid != fid) return state
        val tombstones = state.deletionTombstones + fid
        require(tombstones.size <= MAX_DELETION_TOMBSTONES) {
            "too many pending FCM deletion tombstones"
        }
        val current = state.currentFid.takeUnless { it == fid }
        if (current == state.currentFid && tombstones == state.deletionTombstones) return state
        return PushAddressState(
            currentFid = current,
            deletionTombstones = tombstones,
            revision = state.revision + 1,
            currentObservedAtMs = state.currentObservedAtMs.takeIf { current != null },
            currentExpiresAtMs = state.currentExpiresAtMs.takeIf { current != null },
        )
    }

    fun deletionAcknowledged(state: PushAddressState, fid: String): PushAddressState {
        requireValidFid(fid)
        if (fid !in state.deletionTombstones) return state
        return PushAddressState(
            currentFid = state.currentFid,
            deletionTombstones = state.deletionTombstones - fid,
            revision = state.revision + 1,
            currentObservedAtMs = state.currentObservedAtMs,
            currentExpiresAtMs = state.currentExpiresAtMs,
        )
    }

    fun requireValidFid(fid: String) {
        require(fid.isNotBlank() && fid.length <= MAX_FID_LENGTH && !fid.any(Char::isISOControl)) {
            "invalid opaque FCM installation identifier"
        }
    }

    private const val MAX_FID_LENGTH = 512
    internal const val MAX_DELETION_TOMBSTONES = 32
    internal const val ADDRESS_TTL_MS = 30L * 24 * 60 * 60 * 1_000
}
