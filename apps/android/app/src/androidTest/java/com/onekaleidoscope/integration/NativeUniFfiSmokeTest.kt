package com.onekaleidoscope.integration

import androidx.test.ext.junit.runners.AndroidJUnit4
import kotlinx.coroutines.runBlocking
import org.junit.Assert.assertEquals
import org.junit.Assert.assertSame
import org.junit.Assert.fail
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.kaleido_core.BindingProbeException
import uniffi.kaleido_core.ProjectionProbeCallback
import uniffi.kaleido_core.ProjectionSubscriptionProbe
import uniffi.kaleido_core.asyncBindingProbe
import uniffi.kaleido_core.fallibleBindingProbe
import uniffi.kaleido_core.protocolVersion
import uniffi.kaleido_proto.CanonicalError
import uniffi.kaleido_proto.CommandAck
import uniffi.kaleido_proto.CommandId
import uniffi.kaleido_proto.CommandOutcome
import uniffi.kaleido_proto.Cursor
import uniffi.kaleido_proto.ErrorCode
import uniffi.kaleido_proto.HostId
import uniffi.kaleido_proto.HostReachability
import uniffi.kaleido_proto.ProjectIndexView
import uniffi.kaleido_proto.ProjectionEnvelope
import uniffi.kaleido_proto.ProjectionKey
import uniffi.kaleido_proto.ProjectionPayload

/**
 * Host-independent proof that the test APK loads and executes the packaged Rust library.
 *
 * This class must run unconditionally in Android CI. None of its tests use assumptions,
 * mocks, or a LAN host; every assertion crosses the generated UniFFI boundary.
 */
@RunWith(AndroidJUnit4::class)
class NativeUniFfiSmokeTest {
    @Test
    fun nativeLibraryExecutesCallbackAsyncAndErrorBridges() = runBlocking {
        assertEquals("0.5.0", protocolVersion())

        val error = canonicalError()
        val projection = projectIndexProjection()
        var callbackProjection: ProjectionEnvelope? = null
        var callbackError: CanonicalError? = null
        val subscription = ProjectionSubscriptionProbe()
        try {
            subscription.subscribe(
                object : ProjectionProbeCallback {
                    override fun onProjection(projection: ProjectionEnvelope) {
                        callbackProjection = projection
                    }

                    override fun onError(error: CanonicalError) {
                        callbackError = error
                    }
                },
                projection,
                error,
            )

            assertEquals(projection, callbackProjection)
            assertEquals(error, callbackError)
            subscription.unsubscribe()
        } finally {
            subscription.close()
        }

        val ack = CommandAck(
            commandId = CommandId("native-smoke-command"),
            outcome = CommandOutcome.AcceptedLocally(noteRef = null),
            ackedAtMs = 17L,
        )
        assertEquals(ack, asyncBindingProbe(ack))

        try {
            fallibleBindingProbe(shouldFail = true, error = error)
            fail("the Rust error bridge accepted an explicitly failing probe")
        } catch (probeError: BindingProbeException.Canonical) {
            assertEquals(error, probeError.error)
        }
        assertSame(Unit, fallibleBindingProbe(shouldFail = false, error = error))
    }

    private fun canonicalError() = CanonicalError(
        code = ErrorCode.Internal,
        retriable = false,
        detailRef = null,
        atMs = 7L,
    )

    private fun projectIndexProjection(): ProjectionEnvelope {
        val hostId = HostId("native-smoke-host")
        return ProjectionEnvelope(
            projectionVersion = 1u,
            key = ProjectionKey.ProjectIndex(hostId),
            cursor = Cursor(0uL),
            payload = ProjectionPayload.ProjectIndex(
                ProjectIndexView(
                    hostId = hostId,
                    reachability = HostReachability.OFFLINE,
                    groups = emptyList(),
                ),
            ),
        )
    }
}
