package com.onekaleidoscope.data

import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import uniffi.kaleido_core.MobileConnectionPath
import uniffi.kaleido_core.MobileConnectionStatus

internal sealed interface ConnectionStatusPresentation {
    data class Offline(val reason: String) : ConnectionStatusPresentation
    data object Connecting : ConnectionStatusPresentation
    data class Online(val endpointLabel: String) : ConnectionStatusPresentation
}

internal object ConnectionStatusPresenter {
    fun present(
        status: MobileConnectionStatus,
        zoneId: ZoneId = ZoneId.systemDefault(),
    ): ConnectionStatusPresentation = when (status.path) {
        MobileConnectionPath.OFFLINE -> ConnectionStatusPresentation.Offline(
            "离线 · ${formatTime(status.atMs, zoneId)}",
        )
        MobileConnectionPath.CONNECTING -> ConnectionStatusPresentation.Connecting
        MobileConnectionPath.LAN_DIRECT -> ConnectionStatusPresentation.Online("TLS 1.3 · LAN Direct")
        MobileConnectionPath.PEER_TO_PEER -> ConnectionStatusPresentation.Online("TLS 1.3 · Peer to Peer")
        MobileConnectionPath.RELAYED -> ConnectionStatusPresentation.Online("TLS 1.3 · Self-hosted Relay")
    }

    private fun formatTime(atMs: Long, zoneId: ZoneId): String = runCatching {
        TIME_FORMATTER.format(Instant.ofEpochMilli(atMs).atZone(zoneId))
    }.getOrDefault("时间不可用")

    private val TIME_FORMATTER = DateTimeFormatter.ofPattern("MM-dd HH:mm")
}
