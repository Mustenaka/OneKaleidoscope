package com.onekaleidoscope.network

internal data class NetworkEpochState(
    val epoch: Long,
    val usable: Boolean,
)

/** Orders racy ConnectivityManager callbacks without exposing Network objects to the runtime. */
internal class NetworkEpochTracker<K> {
    private var activeKey: K? = null
    private var state = NetworkEpochState(epoch = 0, usable = false)

    fun available(key: K): NetworkEpochState {
        if (activeKey == key) return state
        activeKey = key
        state = NetworkEpochState(nextEpoch(), usable = false)
        return state
    }

    fun capabilities(key: K, usable: Boolean): NetworkEpochState? {
        if (activeKey != key) return null
        if (state.usable != usable) state = state.copy(usable = usable)
        return state
    }

    fun lost(key: K): NetworkEpochState? {
        if (activeKey != key) return null
        activeKey = null
        state = NetworkEpochState(nextEpoch(), usable = false)
        return state
    }

    private fun nextEpoch(): Long = checkNotNull((state.epoch + 1).takeIf { it > state.epoch }) {
        "network epoch exhausted"
    }
}
