package com.onekaleidoscope.push

import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage

/** FCM is wake-only. It never carries or applies product state. */
class KaleidoFirebaseMessagingService : FirebaseMessagingService() {
    private val vault by lazy(LazyThreadSafetyMode.NONE) { AndroidPushAddressVault(this) }
    private val scheduler by lazy(LazyThreadSafetyMode.NONE) { PushRecoveryScheduler(this) }

    override fun onRegistered(fid: String) {
        vault.recordRegistered(fid)
        MobileSecurityLog.record(MobileSecurityLog.Event.FidRegistered)
        scheduler.scheduleStateChange()
    }

    override fun onUnregistered(fid: String) {
        vault.recordUnregistered(fid)
        MobileSecurityLog.record(MobileSecurityLog.Event.FidUnregistered)
        scheduler.scheduleStateChange()
    }

    override fun onMessageReceived(message: RemoteMessage) {
        when (WakePayload.validate(message.data, message.notification != null)) {
            WakePayloadValidation.Accepted -> {
                MobileSecurityLog.record(MobileSecurityLog.Event.WakeAccepted)
                scheduler.scheduleWake()
            }
            is WakePayloadValidation.Rejected -> {
                MobileSecurityLog.record(MobileSecurityLog.Event.WakeRejected)
            }
        }
    }

    override fun onDeletedMessages() {
        MobileSecurityLog.record(MobileSecurityLog.Event.MessagesDeleted)
        scheduler.scheduleWake()
    }
}
