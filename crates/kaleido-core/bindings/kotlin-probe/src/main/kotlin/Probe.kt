package probe

import uniffi.kaleido_core.BindingProbeException
import uniffi.kaleido_core.ProjectionProbeCallback
import uniffi.kaleido_core.ProjectionSubscriptionProbe
import uniffi.kaleido_core.asyncBindingProbe
import uniffi.kaleido_core.bindingProbe
import uniffi.kaleido_core.fallibleBindingProbe
import uniffi.kaleido_core.MobileQuestionAnswer
import uniffi.kaleido_core.protocolVersion
import uniffi.kaleido_proto.Actor
import uniffi.kaleido_proto.CanonicalError
import uniffi.kaleido_proto.CommandAck
import uniffi.kaleido_proto.Command
import uniffi.kaleido_proto.CommandEnvelope
import uniffi.kaleido_proto.ContentKind
import uniffi.kaleido_proto.ContentRef
import uniffi.kaleido_proto.ContentWriteRequest
import uniffi.kaleido_proto.ContentWriteResponse
import uniffi.kaleido_proto.Cursor
import uniffi.kaleido_proto.DeviceCommandRequest
import uniffi.kaleido_proto.DeviceId
import uniffi.kaleido_proto.HostId
import uniffi.kaleido_proto.ProjectId
import uniffi.kaleido_proto.ProviderRuntimeId
import uniffi.kaleido_proto.ProjectionEnvelope
import uniffi.kaleido_proto.ProjectionKey
import uniffi.kaleido_proto.ProjectionSubscribe
import uniffi.kaleido_proto.ProjectionSubscribeAck
import uniffi.kaleido_proto.ProjectionSubscribeOutcome
import uniffi.kaleido_proto.RuntimeCapabilityView
import uniffi.kaleido_proto.SessionId
import uniffi.kaleido_proto.StateEffect
import uniffi.kaleido_proto.WorkflowId
import uniffi.kaleido_proto.QuestionAnswer
import uniffi.kaleido_proto.QuestionPrompt

fun probeProtocolVersion(): String = protocolVersion()

fun probeCanonicalGraph(
    command: CommandEnvelope,
    projection: ProjectionEnvelope,
    error: CanonicalError?,
    effects: List<StateEffect>,
): ProjectionEnvelope = bindingProbe(command, projection, error, effects)

private class ProjectionProbeSink : ProjectionProbeCallback {
    override fun onProjection(projection: ProjectionEnvelope) {
        consumeProjectionKey(projection.key)
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

fun probeProjectionContracts(
    hostId: HostId,
    projectId: ProjectId,
    sessionId: SessionId,
    workflowId: WorkflowId,
    runtimeId: ProviderRuntimeId,
    cursor: Cursor,
    error: CanonicalError,
): List<ProjectionSubscribeAck> {
    val keys =
        listOf(
            ProjectionKey.ProjectIndex(hostId),
            ProjectionKey.SessionIndex(projectId),
            ProjectionKey.Transcript(sessionId),
            ProjectionKey.LiveActivity(sessionId),
            ProjectionKey.InputQueue(sessionId),
            ProjectionKey.AttentionInbox(hostId),
            ProjectionKey.WorkflowBoard(workflowId),
            ProjectionKey.RuntimeCapability(hostId, runtimeId),
        )
    val outcomes =
        listOf(
            ProjectionSubscribeOutcome.Resumed(cursor),
            ProjectionSubscribeOutcome.CurrentFollows(cursor),
            ProjectionSubscribeOutcome.Rejected(error),
        )

    return keys.mapIndexed { index, key ->
        val request = ProjectionSubscribe(key = key, since = cursor)
        consumeProjectionKey(request.key)
        ProjectionSubscribeAck(key = request.key, outcome = outcomes[index % outcomes.size]).also {
            consumeProjectionOutcome(it.outcome)
        }
    }
}

fun probeMobileIngress(
    deviceId: DeviceId,
    body: Command,
    contentRef: ContentRef,
    error: CanonicalError,
): String {
    val actor: Actor = Actor.Human(deviceId)
    val command = DeviceCommandRequest(idempotencyKey = "probe", ttlMs = 1uL, body = body)
    val write =
        ContentWriteRequest(
            contentKind = ContentKind.PLAIN_TEXT,
            byteLen = contentRef.byteLen,
            digest = contentRef.digest,
        )
    val responses: List<ContentWriteResponse> =
        listOf(ContentWriteResponse.Stored(contentRef), ContentWriteResponse.Rejected(error))

    return buildString {
        append(consumeActor(actor))
        append(command.idempotencyKey)
        append(write.contentKind.name)
        responses.forEach { append(consumeContentWriteResponse(it)) }
    }
}

fun probeRuntimeCapabilityScope(view: RuntimeCapabilityView): String =
    "${view.hostId.value}:${view.runtimeId.value}"

fun probeQuestionSet(prompt: QuestionPrompt, answer: QuestionAnswer, draft: MobileQuestionAnswer): String =
    "${prompt.questionKey}:${answer.questionKey}:${draft.questionKey}:${answer.optionIds.joinToString(",")}"

private fun consumeActor(actor: Actor): String =
    when (actor) {
        is Actor.Human -> actor.deviceId.value
        is Actor.Workflow -> actor.workflowId.value
        Actor.Broker -> "broker"
    }

private fun consumeProjectionKey(key: ProjectionKey): String =
    when (key) {
        is ProjectionKey.ProjectIndex -> key.hostId.value
        is ProjectionKey.SessionIndex -> key.projectId.value
        is ProjectionKey.Transcript -> key.sessionId.value
        is ProjectionKey.LiveActivity -> key.sessionId.value
        is ProjectionKey.InputQueue -> key.sessionId.value
        is ProjectionKey.AttentionInbox -> key.hostId.value
        is ProjectionKey.WorkflowBoard -> key.workflowId.value
        is ProjectionKey.RuntimeCapability -> "${key.hostId.value}:${key.runtimeId.value}"
    }

private fun consumeProjectionOutcome(outcome: ProjectionSubscribeOutcome): String =
    when (outcome) {
        is ProjectionSubscribeOutcome.Resumed -> outcome.fromCursor.seq.toString()
        is ProjectionSubscribeOutcome.CurrentFollows -> outcome.currentCursor.seq.toString()
        is ProjectionSubscribeOutcome.Rejected -> outcome.error.code.toString()
    }

private fun consumeContentWriteResponse(response: ContentWriteResponse): String =
    when (response) {
        is ContentWriteResponse.Stored -> response.contentRef.digest
        is ContentWriteResponse.Rejected -> response.error.code.toString()
    }
