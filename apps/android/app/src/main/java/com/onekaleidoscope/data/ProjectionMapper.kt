package com.onekaleidoscope.data

import com.onekaleidoscope.ui.ActionAvailability
import com.onekaleidoscope.ui.AttentionSubjectUi
import com.onekaleidoscope.ui.AttentionUi
import com.onekaleidoscope.ui.CapabilityStateUi
import com.onekaleidoscope.ui.CapabilityUi
import com.onekaleidoscope.ui.DecisionOptionUi
import com.onekaleidoscope.ui.DecisionToneUi
import com.onekaleidoscope.ui.HostUi
import com.onekaleidoscope.ui.InputQueueUi
import com.onekaleidoscope.ui.LiveActivityUi
import com.onekaleidoscope.ui.ProgressEntryUi
import com.onekaleidoscope.ui.ProjectUi
import com.onekaleidoscope.ui.QueueEntryUi
import com.onekaleidoscope.ui.QueueIntentUi
import com.onekaleidoscope.ui.QueueStateUi
import com.onekaleidoscope.ui.QuestionPromptUi
import com.onekaleidoscope.ui.ReachabilityUi
import com.onekaleidoscope.ui.RuntimeCapabilitiesUi
import com.onekaleidoscope.ui.SessionSectionsUi
import com.onekaleidoscope.ui.SessionStatusUi
import com.onekaleidoscope.ui.SessionUi
import com.onekaleidoscope.ui.TranscriptItemKind
import com.onekaleidoscope.ui.TranscriptItemUi
import com.onekaleidoscope.ui.TranscriptTurnUi
import com.onekaleidoscope.ui.TranscriptUi
import java.time.Instant
import java.time.ZoneId
import java.time.format.DateTimeFormatter
import uniffi.kaleido_core.MobileActionAvailability
import uniffi.kaleido_core.MobileActionBlocker
import uniffi.kaleido_core.MobileTextContent
import uniffi.kaleido_proto.AttentionItem
import uniffi.kaleido_proto.AttentionSubject
import uniffi.kaleido_proto.CapabilityState
import uniffi.kaleido_proto.ContentRef
import uniffi.kaleido_proto.ConnectionFaultReason
import uniffi.kaleido_proto.DecisionOption
import uniffi.kaleido_proto.DecisionSemantics
import uniffi.kaleido_proto.EvidenceSource
import uniffi.kaleido_proto.HostReachability
import uniffi.kaleido_proto.InputQueueView
import uniffi.kaleido_proto.Item
import uniffi.kaleido_proto.ItemBody
import uniffi.kaleido_proto.ItemStatus
import uniffi.kaleido_proto.LiveActivityView
import uniffi.kaleido_proto.LiveBinding
import uniffi.kaleido_proto.OwnershipMode
import uniffi.kaleido_proto.PlanEntryState
import uniffi.kaleido_proto.ProjectIndexView
import uniffi.kaleido_proto.ProviderFamily
import uniffi.kaleido_proto.QueueIntent
import uniffi.kaleido_proto.QueueState
import uniffi.kaleido_proto.RuntimeCapabilityView
import uniffi.kaleido_proto.SessionIndexView
import uniffi.kaleido_proto.SessionStatus
import uniffi.kaleido_proto.SessionSummary
import uniffi.kaleido_proto.TranscriptView
import uniffi.kaleido_proto.TurnOrigin

internal object ProjectionMapper {
    private val timeFormatter = DateTimeFormatter.ofPattern("MM-dd HH:mm")

    fun host(hostId: String, view: ProjectIndexView): HostUi = HostUi(
        id = hostId,
        displayName = "PC ${hostId.takeLast(8)}",
        platform = "PC",
        reachability = when (view.reachability) {
            HostReachability.OFFLINE -> ReachabilityUi.Offline
            HostReachability.LAN_DIRECT -> ReachabilityUi.LanDirect
            HostReachability.PEER_TO_PEER -> ReachabilityUi.PeerToPeer
            HostReachability.RELAYED -> ReachabilityUi.Relayed
        },
        lastSeenLabel = if (view.reachability == HostReachability.OFFLINE) "离线" else "刚刚",
    )

    fun projects(view: ProjectIndexView): List<ProjectUi> = view.groups.flatMap { group ->
        group.projects.map { project ->
            ProjectUi(
                id = project.projectId.value,
                displayName = project.displayName,
                providerLabel = provider(group.family),
                sessionTotal = project.sessionCounts.total.toInt(),
                runningCount = project.sessionCounts.running.toInt(),
                waitingHumanCount = project.sessionCounts.waitingHuman.toInt(),
                attentionCount = project.attentionCount.toInt(),
                lastActivityLabel = time(project.lastActivityAtMs),
            )
        }
    }

    fun projectRuntimeIds(view: ProjectIndexView): Map<String, String> = buildMap {
        view.groups.forEach { group ->
            group.projects.forEach { project ->
                project.bindings.firstOrNull()?.runtimeId?.value?.let { runtime ->
                    put(project.projectId.value, runtime)
                }
            }
        }
    }

    fun projectBindingRuntimeIds(view: ProjectIndexView): Map<String, String> = buildMap {
        view.groups.forEach { group ->
            group.projects.forEach { project ->
                project.bindings.forEach { binding ->
                    put(binding.bindingId.value, binding.runtimeId.value)
                }
            }
        }
    }

    fun sessions(view: SessionIndexView): Pair<SessionSectionsUi, Map<String, SessionSummary>> {
        val all = view.active + view.history + view.archived
        val byId = all.associateBy { it.sessionId.value }
        return SessionSectionsUi(
            active = view.active.map(::session),
            history = view.history.map(::session),
            archived = view.archived.map(::session),
        ) to byId
    }

    private fun session(value: SessionSummary): SessionUi = SessionUi(
        id = value.sessionId.value,
        title = value.title ?: "未命名会话",
        status = when (value.status) {
            SessionStatus.OFFLINE -> SessionStatusUi.Offline
            SessionStatus.IDLE -> SessionStatusUi.Idle
            SessionStatus.RUNNING -> SessionStatusUi.Running
            SessionStatus.WAITING_USER -> SessionStatusUi.WaitingUser
            SessionStatus.WAITING_APPROVAL -> SessionStatusUi.WaitingApproval
            SessionStatus.QUEUED -> SessionStatusUi.Queued
            SessionStatus.FAILED -> SessionStatusUi.Failed
            SessionStatus.COMPLETED -> SessionStatusUi.Completed
            SessionStatus.CANCELLED -> SessionStatusUi.Cancelled
        },
        ownershipLabel = when (value.ownership) {
            OwnershipMode.BROKER_MANAGED -> "Broker 管理"
            OwnershipMode.PROVIDER_MANAGED -> "Provider SDK 管理"
            OwnershipMode.SHARED_RUNTIME -> "共享 Runtime"
            OwnershipMode.EXTERNAL_NATIVE -> "外部会话"
        },
        liveBindingLabel = when (value.liveBinding) {
            is LiveBinding.Controlling -> "可控制"
            is LiveBinding.Observing -> "实时观察"
            is LiveBinding.Blocked -> "连接受阻"
            is LiveBinding.NotBound -> "未实时连接"
        },
        isLive = value.liveBinding is LiveBinding.Controlling || value.liveBinding is LiveBinding.Observing,
        queueDepth = value.queueDepth.toInt(),
        openAttentionCount = value.openAttentionCount.toInt(),
        lastActivityLabel = time(value.lastActivityAtMs),
        runtimeId = runtimeId(value.liveBinding),
    )

    fun transcript(
        view: TranscriptView,
        readText: (ContentRef) -> TextResult,
    ): Pair<TranscriptUi, Map<String, TranscriptItemUi>> {
        val itemsById = mutableMapOf<String, TranscriptItemUi>()
        val turns = view.turns.map { transcriptTurn ->
            val mappedItems = transcriptTurn.items.map { item ->
                item(item, readText).also { itemsById[item.id.value] = it }
            }
            TranscriptTurnUi(
                id = transcriptTurn.turn.id.value,
                statusLabel = transcriptTurn.turn.status.name.lowercase().replace('_', ' '),
                originLabel = when (transcriptTurn.turn.origin) {
                    TurnOrigin.LocalSurface -> "PC 本地"
                    is TurnOrigin.RemoteCommand -> "手机命令"
                    is TurnOrigin.WorkflowStep -> "工作流"
                },
                items = mappedItems,
                errorSummary = transcriptTurn.turn.error?.let { "回合失败" },
            )
        }
        return TranscriptUi(view.sessionId.value, turns, view.hasEarlier) to itemsById
    }

    private fun item(value: Item, readText: (ContentRef) -> TextResult): TranscriptItemUi {
        val (kind, result) = when (val body = value.body) {
            is ItemBody.UserMessage -> TranscriptItemKind.User to readText(body.content)
            is ItemBody.AgentMessage -> TranscriptItemKind.Agent to readText(body.content)
            is ItemBody.Reasoning -> TranscriptItemKind.Reasoning to readText(body.content)
            is ItemBody.ToolCall -> TranscriptItemKind.Tool to (
                body.output?.let(readText)
                    ?: body.arguments?.let(readText)
                    ?: TextResult(body.tool.name, null)
                )
            is ItemBody.FileEdit -> TranscriptItemKind.FileEdit to TextResult(
                "${body.changeSet.entries.size} 个文件变更" + if (body.changeSet.truncated) "（已截断）" else "",
                null,
            )
            is ItemBody.PlanUpdate -> TranscriptItemKind.Plan to TextResult(
                body.entries.joinToString("\n") { "${it.state.name}: ${readText(it.titleRef).text ?: "内容不可用"}" },
                null,
            )
            is ItemBody.TaskUpdate -> TranscriptItemKind.Task to TextResult(
                body.tasks.joinToString("\n") { "${it.state.name}: ${readText(it.titleRef).text ?: "内容不可用"}" },
                null,
            )
            is ItemBody.Diagnostic -> TranscriptItemKind.Diagnostic to readText(body.detail)
        }
        return TranscriptItemUi(
            id = value.id.value,
            kind = kind,
            statusLabel = when (value.status) {
                ItemStatus.PENDING -> "等待"
                ItemStatus.IN_PROGRESS -> "进行中"
                ItemStatus.COMPLETED -> "完成"
                ItemStatus.DECLINED -> "已拒绝"
                ItemStatus.FAILED -> "失败"
                ItemStatus.CANCELLED -> "取消"
            },
            text = result.text,
            contentUnavailableReason = result.unavailable,
        )
    }

    fun liveActivity(
        view: LiveActivityView,
        knownItems: Map<String, TranscriptItemUi>,
        readText: (ContentRef) -> TextResult,
    ): LiveActivityUi = LiveActivityUi(
        sessionId = view.sessionId.value,
        activeTurnId = view.activeTurnId?.value,
        streamingItems = view.streamingItemIds.map { id ->
            knownItems[id.value] ?: TranscriptItemUi(
                id = id.value,
                kind = TranscriptItemKind.Unknown,
                statusLabel = "流式更新",
                text = null,
                contentUnavailableReason = "等待 Transcript 投影确认活动项类型",
            )
        },
        plan = view.plan.map { ProgressEntryUi(readText(it.titleRef).text ?: "内容不可用", planState(it.state)) },
        tasks = view.tasks.map { ProgressEntryUi(readText(it.titleRef).text ?: "内容不可用", planState(it.state)) },
        updatedLabel = time(view.updatedAtMs),
    )

    fun queue(view: InputQueueView, readText: (ContentRef) -> TextResult): InputQueueUi = InputQueueUi(
        sessionId = view.sessionId.value,
        entries = view.entries.map { entry ->
            val text = readText(entry.body)
            QueueEntryUi(
                id = entry.id.value,
                position = entry.position.toInt(),
                intent = when (entry.intent) {
                    QueueIntent.NEW_TURN -> QueueIntentUi.NewTurn
                    QueueIntent.STEER_ACTIVE_TURN -> QueueIntentUi.SteerActiveTurn
                },
                bodyText = text.text,
                bodyUnavailableReason = text.unavailable,
                state = when (entry.state) {
                    QueueState.Pending -> QueueStateUi.Pending
                    is QueueState.Submitting -> QueueStateUi.Submitting
                    is QueueState.DeliveredAsNewTurn -> QueueStateUi.DeliveredNewTurn
                    is QueueState.DeliveredAsSteer -> QueueStateUi.DeliveredSteer
                    is QueueState.Rejected -> QueueStateUi.Rejected
                    is QueueState.Cancelled -> QueueStateUi.Cancelled
                },
                editable = entry.editable,
            )
        },
        writable = view.writable,
        writableReason = if (view.writable) null else "Broker 投影当前标记队列只读",
        steerSupported = view.steerSupported,
    )

    fun attention(
        item: AttentionItem,
        projectName: String,
        sessionTitle: String?,
        availability: ActionAvailability,
        readText: (ContentRef) -> TextResult,
    ): AttentionUi = AttentionUi(
        id = item.id.value,
        projectName = projectName,
        sessionTitle = sessionTitle,
        subject = when (val subject = item.subject) {
            is AttentionSubject.Approval -> AttentionSubjectUi.Approval(
                summary = readText(subject.request.summaryRef).text,
                detail = subject.request.detailRef?.let(readText)?.text,
                joinWarning = when (subject.request.join) {
                    is uniffi.kaleido_proto.JoinState.Joined -> null
                    is uniffi.kaleido_proto.JoinState.Unjoined -> "尚未关联到具体操作项"
                },
                options = subject.request.options.map(::option),
            )
            is AttentionSubject.Question -> AttentionSubjectUi.Question(
                questions = subject.request.questions.map { question ->
                    QuestionPromptUi(
                        key = question.questionKey,
                        prompt = readText(question.promptRef).text,
                        options = question.options.map(::option),
                        multiSelect = question.multiSelect,
                        freeFormAllowed = question.freeFormAllowed,
                    )
                },
            )
            is AttentionSubject.WorkflowGate -> AttentionSubjectUi.Question(
                prompt = readText(subject.request.promptRef).text,
                options = subject.request.options.map(::option),
                freeFormAllowed = subject.request.freeFormAllowed,
            )
            is AttentionSubject.ConnectionFault -> AttentionSubjectUi.ConnectionFault(
                runtimeLabel = subject.runtimeId.value,
                safeReason = when (val reason = subject.reason) {
                    is ConnectionFaultReason.ProcessExited -> "Runtime 已退出${reason.exitCode?.let { "（$it）" }.orEmpty()}"
                    ConnectionFaultReason.HandshakeRejected -> "握手被拒绝"
                    ConnectionFaultReason.AuthRequired -> "需要重新认证"
                    ConnectionFaultReason.Timeout -> "连接超时"
                    ConnectionFaultReason.TransportError -> "传输中断"
                    ConnectionFaultReason.ProtocolViolation -> "协议校验失败"
                },
            )
        },
        expiresLabel = item.expiresAtMs?.let(::time),
        responseAvailability = availability,
    )

    fun capabilities(view: RuntimeCapabilityView): RuntimeCapabilitiesUi = RuntimeCapabilitiesUi(
        runtimeId = view.runtimeId.value,
        runtimeLabel = view.runtimeId.value.takeLast(12),
        negotiatedLabel = time(view.negotiatedAtMs),
        entries = view.entries.map { entry ->
            CapabilityUi(
                id = entry.capability.name,
                displayName = entry.capability.name.lowercase().replace('_', ' '),
                state = when (entry.state) {
                    CapabilityState.Supported -> CapabilityStateUi.Supported
                    CapabilityState.Unsupported -> CapabilityStateUi.Unsupported
                    is CapabilityState.UnavailableOnThisConnection -> CapabilityStateUi.Unavailable
                    CapabilityState.NotVerified -> CapabilityStateUi.NotVerified
                    is CapabilityState.UpstreamBlocked -> CapabilityStateUi.UpstreamBlocked
                },
                reason = when (val state = entry.state) {
                    CapabilityState.Supported -> "已由结构化协议证据确认"
                    CapabilityState.Unsupported -> "Runtime 明确不支持"
                    is CapabilityState.UnavailableOnThisConnection -> state.reason.name
                    CapabilityState.NotVerified -> "尚未获得可验证证据"
                    is CapabilityState.UpstreamBlocked -> "上游阻塞：${state.blockerId.value}"
                },
                evidenceLabel = when (entry.evidence.source) {
                    EvidenceSource.HANDSHAKE_DECLARED -> "握手声明"
                    EvidenceSource.OBSERVED_IN_TRAFFIC -> "实时流量"
                    EvidenceSource.RECORDED_FIXTURE -> "录制证据"
                    EvidenceSource.MANUAL_ACCEPTANCE -> "人工验收"
                    EvidenceSource.ABSENT -> "无证据"
                },
            )
        },
    )

    fun actionAvailability(value: MobileActionAvailability): ActionAvailability = if (value.enabled) {
        ActionAvailability.Enabled
    } else {
        ActionAvailability.disabled(
            when (value.blocker) {
                MobileActionBlocker.SESSION_NOT_LIVE -> "会话当前没有实时连接"
                MobileActionBlocker.RUNTIME_CAPABILITY_MISSING -> "尚未收到 Runtime 能力投影"
                MobileActionBlocker.CAPABILITY_UNSUPPORTED -> "Runtime 明确不支持此操作"
                MobileActionBlocker.CAPABILITY_UNAVAILABLE -> "此连接暂时不可执行该操作"
                MobileActionBlocker.CAPABILITY_NOT_VERIFIED -> "能力尚未验证，不能乐观执行"
                MobileActionBlocker.CAPABILITY_UPSTREAM_BLOCKED -> "上游尚无公开控制路径"
                MobileActionBlocker.QUEUE_UNAVAILABLE -> "队列当前只读"
                MobileActionBlocker.ATTENTION_NOT_REPLYABLE -> "此提醒不能直接回答"
                null -> "当前不可用"
            },
        )
    }

    fun text(value: MobileTextContent): TextResult = when (value) {
        is MobileTextContent.Available -> TextResult(value.text, null)
        is MobileTextContent.TooLarge -> TextResult(null, "正文过大（${value.byteLen} 字节）")
        is MobileTextContent.Unavailable -> TextResult(null, "正文不可用：${value.reason.name}")
    }

    private fun option(value: DecisionOption): DecisionOptionUi = DecisionOptionUi(
        id = value.optionId,
        label = value.label,
        tone = when (value.semantics) {
            DecisionSemantics.ALLOW, DecisionSemantics.ALLOW_ALWAYS -> DecisionToneUi.Positive
            DecisionSemantics.DENY, DecisionSemantics.DENY_ALWAYS, DecisionSemantics.CANCEL -> DecisionToneUi.Destructive
            DecisionSemantics.CHOOSE -> DecisionToneUi.Neutral
        },
    )

    private fun provider(value: ProviderFamily): String = when (value) {
        ProviderFamily.CODEX -> "Codex"
        ProviderFamily.CLAUDE_CODE -> "Claude Code"
        ProviderFamily.OPEN_CODE -> "OpenCode"
        ProviderFamily.ACP -> "ACP"
    }

    private fun runtimeId(value: LiveBinding): String? = when (value) {
        is LiveBinding.Controlling -> value.runtimeId.value
        is LiveBinding.Observing -> value.runtimeId.value
        is LiveBinding.Blocked, is LiveBinding.NotBound -> null
    }

    private fun planState(value: PlanEntryState): String = value.name.lowercase().replace('_', ' ')

    private fun time(epochMs: Long): String = runCatching {
        Instant.ofEpochMilli(epochMs).atZone(ZoneId.systemDefault()).format(timeFormatter)
    }.getOrDefault("未知时间")
}

internal data class TextResult(val text: String?, val unavailable: String?)
