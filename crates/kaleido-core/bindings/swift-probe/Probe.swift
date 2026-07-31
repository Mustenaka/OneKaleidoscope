import Foundation

func probeProtocolVersion() -> String {
    protocolVersion()
}

func probeCanonicalGraph(
    command: CommandEnvelope,
    projection: ProjectionEnvelope,
    error: CanonicalError?,
    effects: [StateEffect]
) -> ProjectionEnvelope {
    bindingProbe(
        command: command,
        projection: projection,
        error: error,
        effects: effects
    )
}

private final class ProjectionProbeSink: ProjectionProbeCallback {
    func onProjection(projection: ProjectionEnvelope) {
        _ = projection
    }

    func onError(error: CanonicalError) {
        _ = error
    }
}

func probeSubscription(
    projection: ProjectionEnvelope,
    error: CanonicalError
) {
    let subscription = ProjectionSubscriptionProbe()
    subscription.subscribe(
        callback: ProjectionProbeSink(),
        projection: projection,
        error: error
    )
    subscription.unsubscribe()
}

func probeFallibleCall(error: CanonicalError) -> CanonicalError? {
    do {
        try fallibleBindingProbe(shouldFail: true, error: error)
        return nil
    } catch let BindingProbeError.Canonical(error: canonicalError) {
        return canonicalError
    } catch {
        return nil
    }
}

func probeAsyncCall(ack: CommandAck) async -> CommandAck {
    await asyncBindingProbe(ack: ack)
}
