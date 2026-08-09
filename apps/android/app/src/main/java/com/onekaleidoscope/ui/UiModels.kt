package com.onekaleidoscope.ui

/** Presentation-only state. Protocol validation and capability decisions stay in kaleido-core. */
data class AppUiState(
    val connection: ConnectionUiState = ConnectionUiState.Unpaired,
    val hosts: List<HostUi> = emptyList(),
    val selectedHostId: String? = null,
    val projects: PanelState<List<ProjectUi>> = PanelState(),
    val selectedProjectId: String? = null,
    val sessions: PanelState<SessionSectionsUi> = PanelState(),
    val selectedSessionId: String? = null,
    val transcript: PanelState<TranscriptUi> = PanelState(),
    val liveActivity: PanelState<LiveActivityUi> = PanelState(),
    val queue: PanelState<InputQueueUi> = PanelState(),
    val attention: PanelState<List<AttentionUi>> = PanelState(),
    val capabilities: PanelState<RuntimeCapabilitiesUi> = PanelState(),
    val selectedRuntimeId: String? = null,
    val draft: String = "",
    val promptAction: ActionAvailability = ActionAvailability.disabled("选择一个会话后可发送"),
    val enqueueNewTurnAction: ActionAvailability = ActionAvailability.disabled("选择一个会话后可排队"),
    val enqueueSteerAction: ActionAvailability = ActionAvailability.disabled("当前会话不支持引导"),
    val attentionDrafts: Map<String, String> = emptyMap(),
    val message: UiMessage? = null,
)

sealed interface ConnectionUiState {
    data object Initializing : ConnectionUiState
    data object Unpaired : ConnectionUiState
    data class Pairing(val stage: String) : ConnectionUiState
    data class Connecting(val hostName: String) : ConnectionUiState
    data class Live(val hostName: String, val endpointLabel: String) : ConnectionUiState
    data class Offline(val hostName: String?, val reason: String, val cachedDataAvailable: Boolean) : ConnectionUiState
    data class Revoked(val hostName: String?, val reason: String) : ConnectionUiState
    data class Error(val safeSummary: String, val retryable: Boolean) : ConnectionUiState
}

data class PanelState<T>(
    val value: T? = null,
    val loading: Boolean = false,
    val freshness: DataFreshness = DataFreshness.Live,
    val error: String? = null,
)

enum class DataFreshness { Live, CachedOffline }

data class ActionAvailability(val enabled: Boolean, val disabledReason: String? = null) {
    init {
        require(enabled || !disabledReason.isNullOrBlank()) {
            "A disabled action needs a user-visible reason"
        }
    }

    companion object {
        val Enabled = ActionAvailability(enabled = true)
        fun disabled(reason: String) = ActionAvailability(enabled = false, disabledReason = reason)
    }
}

data class HostUi(
    val id: String,
    val displayName: String,
    val platform: String,
    val reachability: ReachabilityUi,
    val lastSeenLabel: String,
)

enum class ReachabilityUi { Offline, LanDirect, PeerToPeer, Relayed }

data class ProjectUi(
    val id: String,
    val displayName: String,
    val providerLabel: String,
    val sessionTotal: Int,
    val runningCount: Int,
    val waitingHumanCount: Int,
    val attentionCount: Int,
    val lastActivityLabel: String,
)

data class SessionSectionsUi(
    val active: List<SessionUi> = emptyList(),
    val history: List<SessionUi> = emptyList(),
    val archived: List<SessionUi> = emptyList(),
)

data class SessionUi(
    val id: String,
    val title: String,
    val status: SessionStatusUi,
    val ownershipLabel: String,
    val liveBindingLabel: String,
    val isLive: Boolean,
    val queueDepth: Int,
    val openAttentionCount: Int,
    val lastActivityLabel: String,
    val runtimeId: String?,
)

enum class SessionStatusUi {
    Offline,
    Idle,
    Running,
    WaitingUser,
    WaitingApproval,
    Queued,
    Failed,
    Completed,
    Cancelled,
}

data class TranscriptUi(
    val sessionId: String,
    val turns: List<TranscriptTurnUi>,
    val hasEarlier: Boolean,
)

data class TranscriptTurnUi(
    val id: String,
    val statusLabel: String,
    val originLabel: String,
    val items: List<TranscriptItemUi>,
    val errorSummary: String? = null,
)

data class TranscriptItemUi(
    val id: String,
    val kind: TranscriptItemKind,
    val statusLabel: String,
    val text: String?,
    val contentUnavailableReason: String? = null,
)

enum class TranscriptItemKind { Unknown, User, Agent, Reasoning, Tool, FileEdit, Plan, Task, Diagnostic }

data class LiveActivityUi(
    val sessionId: String,
    val activeTurnId: String?,
    val streamingItems: List<TranscriptItemUi>,
    val plan: List<ProgressEntryUi>,
    val tasks: List<ProgressEntryUi>,
    val updatedLabel: String,
)

data class ProgressEntryUi(val title: String, val stateLabel: String)

data class InputQueueUi(
    val sessionId: String,
    val entries: List<QueueEntryUi>,
    val writable: Boolean,
    val writableReason: String?,
    val steerSupported: Boolean,
)

data class QueueEntryUi(
    val id: String,
    val position: Int,
    val intent: QueueIntentUi,
    val bodyText: String?,
    val bodyUnavailableReason: String? = null,
    val state: QueueStateUi,
    val editable: Boolean,
)

enum class QueueIntentUi { NewTurn, SteerActiveTurn }
enum class QueueStateUi { Pending, Submitting, DeliveredNewTurn, DeliveredSteer, Rejected, Cancelled }

data class AttentionUi(
    val id: String,
    val projectName: String,
    val sessionTitle: String?,
    val subject: AttentionSubjectUi,
    val expiresLabel: String?,
    val responseAvailability: ActionAvailability,
)

sealed interface AttentionSubjectUi {
    data class Approval(
        val summary: String?,
        val detail: String?,
        val joinWarning: String?,
        val options: List<DecisionOptionUi>,
    ) : AttentionSubjectUi

    data class Question(
        val prompt: String?,
        val options: List<DecisionOptionUi>,
        val freeFormAllowed: Boolean,
    ) : AttentionSubjectUi

    data class ConnectionFault(val runtimeLabel: String, val safeReason: String) : AttentionSubjectUi
}

data class DecisionOptionUi(val id: String, val label: String, val tone: DecisionToneUi)
enum class DecisionToneUi { Positive, Neutral, Destructive }

data class RuntimeCapabilitiesUi(
    val runtimeId: String,
    val runtimeLabel: String,
    val negotiatedLabel: String,
    val entries: List<CapabilityUi>,
)

data class CapabilityUi(
    val id: String,
    val displayName: String,
    val state: CapabilityStateUi,
    val reason: String,
    val evidenceLabel: String,
)

enum class CapabilityStateUi { Supported, Unsupported, Unavailable, NotVerified, UpstreamBlocked }

data class UiMessage(val id: Long, val text: String, val actionLabel: String? = null)

enum class AppDestination { Hosts, Projects, Sessions, Session, Attention, Capabilities }

sealed interface UiAction {
    data class Navigate(val destination: AppDestination) : UiAction
    data class SubmitPairingQr(val qrPayload: String) : UiAction
    data class ConnectHost(val hostId: String) : UiAction
    data class DisconnectHost(val hostId: String) : UiAction
    data class ForgetHost(val hostId: String) : UiAction
    data class SelectHost(val hostId: String) : UiAction
    data class SelectProject(val projectId: String) : UiAction
    data class SelectSession(val sessionId: String) : UiAction
    data class SelectRuntime(val runtimeId: String) : UiAction
    data class UpdateDraft(val value: String) : UiAction
    data object SubmitPrompt : UiAction
    data class EnqueueInput(val intent: QueueIntentUi) : UiAction
    data class RespondAttention(
        val attentionId: String,
        val optionId: String?,
        val freeForm: String?,
    ) : UiAction
    data class UpdateAttentionDraft(val attentionId: String, val value: String) : UiAction
    data object Refresh : UiAction
    data object RetryConnection : UiAction
    data object DismissMessage : UiAction
    data object MessageAction : UiAction
}
