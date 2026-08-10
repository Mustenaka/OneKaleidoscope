package com.onekaleidoscope

import android.app.Application
import android.content.Context
import com.onekaleidoscope.data.MobileRepository
import com.onekaleidoscope.data.PushSyncOutcome
import com.onekaleidoscope.data.RemoteRecoveryTrigger
import com.onekaleidoscope.network.AndroidNetworkMonitor
import com.onekaleidoscope.push.MobileSecurityLog
import com.onekaleidoscope.push.PushAddressState
import com.onekaleidoscope.push.PushRecoveryScheduler
import java.io.Closeable

class OneKaleidoscopeApplication : Application() {
    internal val mobileRuntime by lazy(LazyThreadSafetyMode.SYNCHRONIZED) { MobileRuntime(this) }

    override fun onCreate() {
        super.onCreate()
        check(AndroidIrohJni.install(applicationContext)) {
            "Android native network context initialization failed"
        }
        mobileRuntime.start()
    }
}

/** Process-owned Android lifecycle shell; routing and protocol decisions remain in Rust. */
internal class MobileRuntime(context: Context) : Closeable {
    private val applicationContext = context.applicationContext
    private val lock = Any()
    private var ownedRepository: MobileRepository? = null
    private var lastNetworkEpoch = 0L
    private val networkMonitor = AndroidNetworkMonitor(applicationContext) { state ->
        synchronized(lock) {
            if (state.epoch > lastNetworkEpoch) {
                lastNetworkEpoch = state.epoch
                ownedRepository?.requestRemoteRecovery(RemoteRecoveryTrigger.NetworkChanged(state.epoch))
                MobileSecurityLog.record(MobileSecurityLog.Event.NetworkChanged)
            }
        }
    }

    val repository: MobileRepository
        get() = synchronized(lock) {
            ownedRepository ?: MobileRepository(
                applicationContext,
                onRemoteConfigured = {
                    PushRecoveryScheduler(applicationContext).scheduleStateChange()
                },
            ).also { ownedRepository = it }
        }

    fun start() {
        networkMonitor.start()
        PushRecoveryScheduler(applicationContext).scheduleStateChange()
    }

    fun recoverFromPush() {
        repository.requestRemoteRecovery(RemoteRecoveryTrigger.PushWake)
    }

    suspend fun synchronizePushAddress(state: PushAddressState): PushSyncOutcome =
        repository.synchronizePushAddress(state)

    override fun close() {
        networkMonitor.close()
        synchronized(lock) {
            ownedRepository?.close()
            ownedRepository = null
        }
    }
}

internal fun Context.mobileRuntime(): MobileRuntime =
    (applicationContext as OneKaleidoscopeApplication).mobileRuntime
