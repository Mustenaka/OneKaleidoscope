package com.onekaleidoscope.data

import java.time.ZoneOffset
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import uniffi.kaleido_core.MobileConnectionPath
import uniffi.kaleido_core.MobileConnectionStatus

class ConnectionStatusPresentationTest {
    @Test
    fun connectingIsNotPresentedAsAnOnlinePath() {
        val presentation = ConnectionStatusPresenter.present(
            MobileConnectionStatus(MobileConnectionPath.CONNECTING, 1_000),
            ZoneOffset.UTC,
        )

        assertEquals(ConnectionStatusPresentation.Connecting, presentation)
        assertFalse(presentation is ConnectionStatusPresentation.Online)
    }

    @Test
    fun offlineUsesCoreObservedTimestamp() {
        val presentation = ConnectionStatusPresenter.present(
            MobileConnectionStatus(MobileConnectionPath.OFFLINE, 0),
            ZoneOffset.UTC,
        )

        assertEquals(
            ConnectionStatusPresentation.Offline("离线 · 01-01 00:00"),
            presentation,
        )
    }

    @Test
    fun onlineLabelsDistinguishLanPeerAndSelfHostedRelay() {
        val labels = listOf(
            MobileConnectionPath.LAN_DIRECT,
            MobileConnectionPath.PEER_TO_PEER,
            MobileConnectionPath.RELAYED,
        ).map { path ->
            ConnectionStatusPresenter.present(MobileConnectionStatus(path, 1), ZoneOffset.UTC)
        }.filterIsInstance<ConnectionStatusPresentation.Online>().map { it.endpointLabel }

        assertEquals(3, labels.distinct().size)
        assertTrue(labels.any { it.contains("Self-hosted Relay") })
    }
}
