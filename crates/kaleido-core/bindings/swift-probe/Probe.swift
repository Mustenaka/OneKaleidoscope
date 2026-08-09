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
        _ = consumeProjectionKey(projection.key)
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

func probeProjectionContracts(
    hostId: HostId,
    projectId: ProjectId,
    sessionId: SessionId,
    workflowId: WorkflowId,
    runtimeId: ProviderRuntimeId,
    cursor: Cursor,
    error: CanonicalError
) -> [ProjectionSubscribeAck] {
    let keys: [ProjectionKey] = [
        .projectIndex(hostId: hostId),
        .sessionIndex(projectId: projectId),
        .transcript(sessionId: sessionId),
        .liveActivity(sessionId: sessionId),
        .inputQueue(sessionId: sessionId),
        .attentionInbox(hostId: hostId),
        .workflowBoard(workflowId: workflowId),
        .runtimeCapability(hostId: hostId, runtimeId: runtimeId),
    ]
    let outcomes: [ProjectionSubscribeOutcome] = [
        .resumed(fromCursor: cursor),
        .currentFollows(currentCursor: cursor),
        .rejected(error: error),
    ]

    return keys.enumerated().map { index, key in
        let request = ProjectionSubscribe(key: key, since: cursor)
        _ = consumeProjectionKey(request.key)
        let ack = ProjectionSubscribeAck(
            key: request.key,
            outcome: outcomes[index % outcomes.count]
        )
        _ = consumeProjectionOutcome(ack.outcome)
        return ack
    }
}

func probeMobileIngress(
    deviceId: DeviceId,
    body: Command,
    contentRef: ContentRef,
    error: CanonicalError
) -> String {
    let actor: Actor = .human(deviceId: deviceId)
    let command = DeviceCommandRequest(idempotencyKey: "probe", ttlMs: 1, body: body)
    let write = ContentWriteRequest(
        contentKind: .plainText,
        byteLen: contentRef.byteLen,
        digest: contentRef.digest
    )
    let responses: [ContentWriteResponse] = [
        .stored(contentRef: contentRef),
        .rejected(error: error),
    ]

    return consumeActor(actor)
        + command.idempotencyKey
        + String(describing: write.contentKind)
        + responses.map(consumeContentWriteResponse).joined()
}

func probeRuntimeCapabilityScope(view: RuntimeCapabilityView) -> String {
    "\(view.hostId.value):\(view.runtimeId.value)"
}

private func consumeActor(_ actor: Actor) -> String {
    switch actor {
    case let .human(deviceId):
        return deviceId.value
    case let .workflow(workflowId):
        return workflowId.value
    case .broker:
        return "broker"
    }
}

private func consumeProjectionKey(_ key: ProjectionKey) -> String {
    switch key {
    case let .projectIndex(hostId):
        return hostId.value
    case let .sessionIndex(projectId):
        return projectId.value
    case let .transcript(sessionId):
        return sessionId.value
    case let .liveActivity(sessionId):
        return sessionId.value
    case let .inputQueue(sessionId):
        return sessionId.value
    case let .attentionInbox(hostId):
        return hostId.value
    case let .workflowBoard(workflowId):
        return workflowId.value
    case let .runtimeCapability(hostId, runtimeId):
        return "\(hostId.value):\(runtimeId.value)"
    }
}

private func consumeProjectionOutcome(_ outcome: ProjectionSubscribeOutcome) -> String {
    switch outcome {
    case let .resumed(fromCursor):
        return String(fromCursor.seq)
    case let .currentFollows(currentCursor):
        return String(currentCursor.seq)
    case let .rejected(error):
        return String(describing: error.code)
    }
}

private func consumeContentWriteResponse(_ response: ContentWriteResponse) -> String {
    switch response {
    case let .stored(contentRef):
        return contentRef.digest
    case let .rejected(error):
        return String(describing: error.code)
    }
}
