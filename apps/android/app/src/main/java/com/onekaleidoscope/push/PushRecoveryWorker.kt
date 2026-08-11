package com.onekaleidoscope.push

import android.content.Context
import androidx.work.Constraints
import androidx.work.CoroutineWorker
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.WorkerParameters
import com.onekaleidoscope.mobileRuntime

internal class PushRecoveryScheduler(context: Context) {
    private val workManager = WorkManager.getInstance(context.applicationContext)

    fun scheduleWake() {
        enqueue(ExistingWorkPolicy.KEEP)
    }

    fun scheduleStateChange() {
        enqueue(ExistingWorkPolicy.REPLACE)
    }

    private fun enqueue(policy: ExistingWorkPolicy) {
        val request = OneTimeWorkRequestBuilder<PushRecoveryWorker>()
            .setConstraints(
                Constraints.Builder()
                    .setRequiredNetworkType(NetworkType.CONNECTED)
                    .build(),
            )
            .build()
        workManager.enqueueUniqueWork(UNIQUE_WORK, policy, request)
        MobileSecurityLog.record(MobileSecurityLog.Event.RecoveryScheduled)
    }

    private companion object {
        const val UNIQUE_WORK = "r4-remote-recovery-v1"
    }
}

internal class PushRecoveryWorker(
    context: Context,
    parameters: WorkerParameters,
) : CoroutineWorker(context, parameters) {
    override suspend fun doWork(): Result = try {
        val vault = AndroidPushAddressVault(applicationContext)
        val state = vault.refreshCurrent()
        val runtime = applicationContext.mobileRuntime()
        val outcome = runtime.synchronizePushAddress(state)
        if (outcome.deletionsAcknowledged) {
            state.deletionTombstones.forEach(vault::acknowledgeDeletion)
        }
        runtime.recoverFromPush()
        if (outcome.complete) Result.success() else Result.retry()
    } catch (_: RuntimeException) {
        Result.retry()
    }
}
