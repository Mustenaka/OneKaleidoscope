package com.onekaleidoscope.network

import android.content.Context
import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.os.Handler
import android.os.Looper
import java.io.Closeable
import java.util.concurrent.atomic.AtomicBoolean

internal class AndroidNetworkMonitor(
    context: Context,
    private val onState: (NetworkEpochState) -> Unit,
) : Closeable {
    private val connectivity = context.applicationContext
        .getSystemService(ConnectivityManager::class.java)
    private val tracker = NetworkEpochTracker<Network>()
    private val started = AtomicBoolean(false)
    private val callback = object : ConnectivityManager.NetworkCallback() {
        override fun onAvailable(network: Network) {
            onState(tracker.available(network))
        }

        override fun onCapabilitiesChanged(network: Network, capabilities: NetworkCapabilities) {
            val usable = capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
                capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED)
            tracker.capabilities(network, usable)?.let(onState)
        }

        override fun onLost(network: Network) {
            tracker.lost(network)?.let(onState)
        }
    }

    fun start() {
        if (started.compareAndSet(false, true)) {
            connectivity.registerDefaultNetworkCallback(callback, Handler(Looper.getMainLooper()))
        }
    }

    override fun close() {
        if (started.compareAndSet(true, false)) {
            connectivity.unregisterNetworkCallback(callback)
        }
    }
}
