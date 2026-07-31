package probe

import uniffi.kaleido_core.BindingProbeException
import uniffi.kaleido_core.ProjectionProbeCallback
import uniffi.kaleido_core.ProjectionSubscriptionProbe
import uniffi.kaleido_core.asyncBindingProbe
import uniffi.kaleido_core.bindingProbe
import uniffi.kaleido_core.fallibleBindingProbe
import uniffi.kaleido_core.protocolVersion
import uniffi.kaleido_proto.CanonicalError
import uniffi.kaleido_proto.CommandAck
import uniffi.kaleido_proto.CommandEnvelope
import uniffi.kaleido_proto.ProjectionEnvelope
import uniffi.kaleido_proto.StateEffect

fun probeProtocolVersion(): String = protocolVersion()

fun probeCanonicalGraph(
    command: CommandEnvelope,
    projection: ProjectionEnvelope,
    error: CanonicalError?,
    effects: List<StateEffect>,
): ProjectionEnvelope = bindingProbe(command, projection, error, effects)

private class ProjectionProbeSink : ProjectionProbeCallback {
    override fun onProjection(projection: ProjectionEnvelope) {
        checkNotNull(projection)
    }

    override fun onError(error: CanonicalError) {
        checkNotNull(error)
    }
}

fun probeSubscription(
    projection: ProjectionEnvelope,
    error: CanonicalError,
) {
    val subscription = ProjectionSubscriptionProbe()
    subscription.subscribe(ProjectionProbeSink(), projection, error)
    subscription.unsubscribe()
}

fun probeFallibleCall(error: CanonicalError): CanonicalError? =
    try {
        fallibleBindingProbe(shouldFail = true, error = error)
        null
    } catch (probeError: BindingProbeException.Canonical) {
        probeError.error
    }

suspend fun probeAsyncCall(ack: CommandAck): CommandAck = asyncBindingProbe(ack)
