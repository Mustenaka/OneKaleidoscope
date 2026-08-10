package com.onekaleidoscope.data

import android.content.Context
import com.onekaleidoscope.platform.AndroidCoreStorage
import com.onekaleidoscope.platform.AndroidDeviceSigner
import com.onekaleidoscope.platform.AndroidSecureCredentialVault
import com.onekaleidoscope.ui.ActionAvailability
import com.onekaleidoscope.ui.AppUiState
import com.onekaleidoscope.ui.ConnectionUiState
import com.onekaleidoscope.ui.DataFreshness
import com.onekaleidoscope.ui.HostUi
import com.onekaleidoscope.ui.PanelState
import com.onekaleidoscope.ui.QueueIntentUi
import com.onekaleidoscope.ui.ReachabilityUi
import com.onekaleidoscope.ui.SessionSectionsUi
import com.onekaleidoscope.ui.SessionUi
import com.onekaleidoscope.ui.UiAction
import com.onekaleidoscope.ui.UiMessage
import java.io.Closeable
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import uniffi.kaleido_core.MobileClient
import uniffi.kaleido_core.MobileClientException
import uniffi.kaleido_core.MobileQuestionAnswer
import uniffi.kaleido_core.MobileSessionAction
import uniffi.kaleido_core.ProjectionCallback
import uniffi.kaleido_core.ProjectionSubscription
import uniffi.kaleido_core.mobileAttentionActionAvailability
import uniffi.kaleido_core.mobileSessionActionAvailability
import uniffi.kaleido_proto.AttentionItem
import uniffi.kaleido_proto.CanonicalError
import uniffi.kaleido_proto.CommandAck
import uniffi.kaleido_proto.CommandOutcome
import uniffi.kaleido_proto.HostId
import uniffi.kaleido_proto.InputQueueView
import uniffi.kaleido_proto.LiveActivityView
import uniffi.kaleido_proto.LiveBinding
import uniffi.kaleido_proto.ProjectId
import uniffi.kaleido_proto.ProjectIndexView
import uniffi.kaleido_proto.ProjectionEnvelope
import uniffi.kaleido_proto.ProjectionKey
import uniffi.kaleido_proto.ProjectionPayload
import uniffi.kaleido_proto.ProviderRuntimeId
import uniffi.kaleido_proto.QueueIntent
import uniffi.kaleido_proto.RuntimeCapabilityView
import uniffi.kaleido_proto.SessionId
import uniffi.kaleido_proto.SessionIndexView
import uniffi.kaleido_proto.SessionSummary
import uniffi.kaleido_proto.TranscriptView
import uniffi.kaleido_proto.TurnId

/**
 * Android lifecycle adapter around the Rust mobile core.
 *
 * Every native call, subscription callback and state reduction is serialized on one IO lane.
 * Kotlin never validates cursors or reconstructs canonical state; the extra cursor comparison only
 * prevents a stale UI callback from replacing a newer, already core-validated full projection.
 */
internal class MobileRepository(context: Context) : Closeable {
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO.limitedParallelism(1))
    private val closing = AtomicBoolean(false)
    private val messageIds = AtomicLong(1)

    private var client: MobileClient? = null
    private var connected = false
    private var workerStarted = false
    private var nextSubscriptionGeneration = 1L

    private val subscriptions = mutableMapOf<ProjectionKey, ActiveSubscription>()
    private val projectionHighWater = ProjectionCursorHighWater<ProjectionKey>()
    private val pendingActions = PendingActionTracker()

    private var projectIndexView: ProjectIndexView? = null
    private var projectIndexFreshness = DataFreshness.CachedOffline
    private val projectNames = mutableMapOf<String, String>()
    private val projectRuntimes = mutableMapOf<String, String>()
    private val projectBindingRuntimes = mutableMapOf<String, String>()
    private val hostRuntimeIds = linkedSetOf<String>()

    private val sessionViews = mutableMapOf<String, SessionIndexView>()
    private val sessionFreshness = mutableMapOf<String, DataFreshness>()
    private val sessionsByProject = mutableMapOf<String, Map<String, SessionSummary>>()
    private val sessionsById = mutableMapOf<String, SessionSummary>()

    private val transcriptViews = mutableMapOf<String, TranscriptView>()
    private val transcriptFreshness = mutableMapOf<String, DataFreshness>()
    private val transcriptItems = mutableMapOf<String, Map<String, com.onekaleidoscope.ui.TranscriptItemUi>>()
    private val liveViews = mutableMapOf<String, LiveActivityView>()
    private val liveFreshness = mutableMapOf<String, DataFreshness>()
    private val queueViews = mutableMapOf<String, InputQueueView>()
    private val queueFreshness = mutableMapOf<String, DataFreshness>()

    private val capabilities = mutableMapOf<String, RuntimeCapabilityView>()
    private val capabilityFreshness = mutableMapOf<String, DataFreshness>()
    private var attentionItems: List<AttentionItem> = emptyList()
    private var attentionFreshness = DataFreshness.CachedOffline
    private val ephemeralText = mutableMapOf<String, TextResult>()

    private val mutableState = MutableStateFlow(
        AppUiState(connection = ConnectionUiState.Initializing),
    )
    val state: StateFlow<AppUiState> = mutableState.asStateFlow()

    init {
        val applicationContext = context.applicationContext
        scope.launch { initialize(applicationContext) }
    }

    fun dispatch(action: UiAction) {
        if (closing.get()) return
        scope.launch { handleAction(action) }
    }

    private fun initialize(context: Context) {
        val opened = try {
            MobileClient.newWithSecureVault(
                cacheDirectory = AndroidCoreStorage.projectionCacheDirectory(context).absolutePath,
                signer = AndroidDeviceSigner(),
                credentialVault = AndroidSecureCredentialVault(context),
            )
        } catch (_: MobileClientException) {
            null
        } catch (_: RuntimeException) {
            null
        }
        client = opened
        if (opened == null) {
            mutableState.value = AppUiState(
                connection = ConnectionUiState.Error(
                    safeSummary = "无法初始化安全凭据或 Rust 核心",
                    retryable = false,
                ),
            )
            return
        }
        val paired = try {
            opened.pairedHostInfo()
        } catch (_: MobileClientException) {
            null
        } catch (_: RuntimeException) {
            null
        }
        if (paired == null) {
            mutableState.value = AppUiState(connection = ConnectionUiState.Unpaired)
            return
        }
        val hostId = paired.hostId.value
        mutableState.update {
            it.copy(
                connection = ConnectionUiState.Offline(
                    hostName = hostLabel(hostId),
                    reason = "尚未连接",
                    cachedDataAvailable = false,
                ),
                selectedHostId = hostId,
            )
        }
        hydrateHostCache(hostId)
        updateOfflineCacheFlag()
    }

    private fun handleAction(action: UiAction) {
        when (action) {
            is UiAction.SubmitPairingQr -> pair(action.qrPayload)
            is UiAction.ConnectHost, UiAction.RetryConnection -> connect()
            UiAction.Refresh -> if (connected) {
                reconcileSubscriptions()
                showMessage("已连接；实时投影会自动更新")
            } else {
                connect()
            }
            is UiAction.DisconnectHost -> disconnect()
            is UiAction.SelectHost -> mutableState.update { it.copy(selectedHostId = action.hostId) }
            is UiAction.SelectProject -> selectProject(action.projectId)
            is UiAction.SelectSession -> selectSession(action.sessionId)
            is UiAction.SelectRuntime -> selectRuntime(action.runtimeId)
            is UiAction.UpdateDraft -> mutableState.update { it.copy(draft = action.value) }
            UiAction.SubmitPrompt -> submitPrompt()
            UiAction.ResumeSession -> resumeSession()
            UiAction.InterruptTurn -> interruptTurn()
            is UiAction.EnqueueInput -> enqueue(action.intent)
            is UiAction.UpdateAttentionDraft -> mutableState.update {
                it.copy(attentionDrafts = it.attentionDrafts + (action.attentionId to action.value))
            }
            is UiAction.RespondAttention -> respondAttention(action)
            is UiAction.RespondQuestion -> respondQuestion(action)
            is UiAction.ForgetHost -> showMessage("请先在 hostd 撤销此设备；本版本不在本地静默遗忘凭据")
            UiAction.DismissMessage -> mutableState.update { it.copy(message = null) }
            UiAction.MessageAction, is UiAction.Navigate -> Unit
        }
    }

    private fun pair(payload: String) {
        val active = client ?: return
        if (payload.isBlank()) {
            showMessage("配对内容不能为空")
            return
        }
        mutableState.update { it.copy(connection = ConnectionUiState.Pairing("验证 PC 身份")) }
        try {
            active.pair(payload, "OneKaleidoscope Android")
            val paired = active.pairedHostInfo() ?: throw IllegalStateException("missing paired host")
            mutableState.update { it.copy(selectedHostId = paired.hostId.value) }
            workerStarted = false
            connectNow(active, paired.hostId.value)
        } catch (_: MobileClientException) {
            mutableState.update {
                it.copy(connection = ConnectionUiState.Error("配对失败：二维码、Host pin 或配对凭据无效", true))
            }
        } catch (_: RuntimeException) {
            mutableState.update { it.copy(connection = ConnectionUiState.Error("配对失败", true)) }
        }
    }

    private fun connect() {
        if (connected) return
        val active = client ?: return
        val hostId = try {
            active.pairedHostInfo()?.hostId?.value
        } catch (_: MobileClientException) {
            null
        } catch (_: RuntimeException) {
            null
        }
        if (hostId == null) {
            mutableState.update { it.copy(connection = ConnectionUiState.Unpaired) }
            return
        }
        connectNow(active, hostId)
    }

    private fun connectNow(active: MobileClient, hostId: String) {
        mutableState.update { it.copy(connection = ConnectionUiState.Connecting(hostLabel(hostId))) }
        discardSubscriptionHandles()
        try {
            if (workerStarted) active.reconnect() else active.connect()
            workerStarted = true
            connected = true
            mutableState.update {
                it.copy(
                    connection = ConnectionUiState.Live(hostLabel(hostId), "TLS 1.3 · LAN"),
                    selectedHostId = hostId,
                )
            }
            reconcileSubscriptions()
            // A successful transport connection is not projection freshness evidence. Each panel
            // remains cached until its own CurrentFollows/Resumed callback is reduced below.
            renderAllSelected()
        } catch (_: MobileClientException.Authentication) {
            connected = false
            workerStarted = false
            mutableState.update {
                it.copy(connection = ConnectionUiState.Revoked(hostLabel(hostId), "认证失败、Host pin 改变或设备已吊销"))
            }
            disableAllActions("设备认证失败")
        } catch (_: MobileClientException) {
            markOffline("无法连接 PC")
        } catch (_: RuntimeException) {
            markOffline("无法连接 PC")
        }
    }

    private fun disconnect() {
        val active = client ?: return
        connected = false
        closeSubscriptions(sendUnsubscribe = true)
        runCatching { active.disconnect() }
        workerStarted = false
        markOffline("已断开连接")
    }

    private fun hydrateHostCache(hostId: String) {
        val active = client ?: return
        listOf(
            ProjectionKey.ProjectIndex(HostId(hostId)),
            ProjectionKey.AttentionInbox(HostId(hostId)),
        ).forEach { key ->
            runCatching { active.cachedProjection(key) }
                .getOrNull()
                ?.let { applyCachedProjection(key, it) }
        }
    }

    private fun selectProject(projectId: String) {
        mutableState.update {
            it.copy(
                selectedProjectId = projectId,
                selectedSessionId = null,
                selectedRuntimeId = projectRuntimes[projectId],
                sessions = PanelState(loading = true),
                transcript = PanelState(),
                liveActivity = PanelState(),
                queue = PanelState(),
            )
        }
        renderSelectedSessions()
        reconcileSubscriptions()
        val key = ProjectionKey.SessionIndex(ProjectId(projectId))
        loadCachedProjection(key)
        projectRuntimes[projectId]?.let(::selectRuntime)
        reconcileSubscriptions()
        refreshActionAvailability()
    }

    private fun selectSession(sessionId: String) {
        val session = sessionsById[sessionId]
        val runtimeId = liveRuntimeId(session?.liveBinding)
            ?: session?.projectBindingId?.value?.let(projectBindingRuntimes::get)
            ?: mutableState.value.selectedRuntimeId
        mutableState.update {
            it.copy(
                selectedSessionId = sessionId,
                selectedRuntimeId = runtimeId,
                transcript = PanelState(loading = true),
                liveActivity = PanelState(loading = true),
                queue = PanelState(loading = true),
            )
        }
        reconcileSubscriptions()
        selectedSessionKeys(sessionId).forEach(::loadCachedProjection)
        renderTranscript(sessionId)
        renderLive(sessionId)
        renderQueue(sessionId)
        runtimeId?.let(::selectRuntime)
        reconcileSubscriptions()
        refreshActionAvailability()
    }

    private fun selectRuntime(runtimeId: String) {
        mutableState.update {
            it.copy(
                selectedRuntimeId = runtimeId,
                capabilities = capabilities[runtimeId]?.let { view ->
                    PanelState(
                        ProjectionMapper.capabilities(view),
                        freshness = capabilityFreshness[runtimeId] ?: DataFreshness.CachedOffline,
                    )
                } ?: PanelState(loading = connected),
            )
        }
        val hostId = mutableState.value.selectedHostId ?: return
        loadCachedProjection(ProjectionKey.RuntimeCapability(HostId(hostId), ProviderRuntimeId(runtimeId)))
        reconcileSubscriptions()
    }

    private fun loadCachedProjection(key: ProjectionKey) {
        val active = client ?: return
        val cached = runCatching { active.cachedProjection(key) }.getOrNull() ?: return
        applyCachedProjection(key, cached)
    }

    private fun applyCachedProjection(key: ProjectionKey, cached: ProjectionEnvelope) {
        if (!projectionHighWater.acceptCached(key, cached.cursor.seq)) {
            return
        }
        processProjection(cached, DataFreshness.CachedOffline)
    }

    private fun reconcileSubscriptions() {
        if (!connected) return
        val desired = desiredSubscriptionKeys()
        (subscriptions.keys - desired).toList().forEach(::unsubscribeKey)
        for (key in desired) {
            if (!connected) break
            if (key !in subscriptions) subscribeKey(key)
        }
    }

    private fun desiredSubscriptionKeys(): LinkedHashSet<ProjectionKey> {
        val desired = linkedSetOf<ProjectionKey>()
        val hostId = mutableState.value.selectedHostId ?: return desired
        val host = HostId(hostId)
        desired += ProjectionKey.ProjectIndex(host)
        desired += ProjectionKey.AttentionInbox(host)
        hostRuntimeIds.forEach { desired += ProjectionKey.RuntimeCapability(host, ProviderRuntimeId(it)) }
        mutableState.value.selectedProjectId?.let { desired += ProjectionKey.SessionIndex(ProjectId(it)) }
        attentionItems.map { it.projectId.value }.distinct().forEach {
            desired += ProjectionKey.SessionIndex(ProjectId(it))
        }
        mutableState.value.selectedSessionId?.let { desired += selectedSessionKeys(it) }
        return desired
    }

    private fun selectedSessionKeys(sessionId: String): List<ProjectionKey> = listOf(
        ProjectionKey.Transcript(SessionId(sessionId)),
        ProjectionKey.LiveActivity(SessionId(sessionId)),
        ProjectionKey.InputQueue(SessionId(sessionId)),
    )

    private fun subscribeKey(key: ProjectionKey) {
        val active = client ?: return
        val generation = nextSubscriptionGeneration++
        try {
            val subscription = active.subscribe(key, object : ProjectionCallback {
                override fun onProjection(projection: ProjectionEnvelope) {
                    enqueueCallback { handleProjection(key, generation, projection) }
                }

                override fun onError(error: CanonicalError) {
                    enqueueCallback { handleSubscriptionError(key, generation, error) }
                }

                override fun onClosed(error: CanonicalError?) {
                    enqueueCallback { handleSubscriptionClosed(key, generation, error) }
                }
            })
            subscriptions[key] = ActiveSubscription(generation, subscription)
            val synchronized = active.cachedProjection(key)
                ?: throw IllegalStateException("synchronized projection cache missing")
            applySynchronizedProjection(key, synchronized)
        } catch (_: MobileClientException) {
            handleSubscriptionFailure()
        } catch (_: RuntimeException) {
            handleSubscriptionFailure()
        }
    }

    private fun handleSubscriptionFailure() {
        connected = false
        closeSubscriptions(sendUnsubscribe = true)
        markOffline("订阅中断，将从最近游标恢复")
    }

    private fun enqueueCallback(block: () -> Unit) {
        if (closing.get()) return
        scope.launch { block() }
    }

    private fun handleProjection(key: ProjectionKey, generation: Long, projection: ProjectionEnvelope) {
        if (subscriptions[key]?.generation != generation || !connected) return
        if (!projectionHighWater.acceptLive(key, projection.cursor.seq)) {
            showMessage("已忽略过期投影；继续等待最新状态")
            return
        }
        processProjection(projection, DataFreshness.Live)
    }

    private fun applySynchronizedProjection(key: ProjectionKey, projection: ProjectionEnvelope) {
        val freshness = projectionHighWater.synchronizedFreshness(key, projection.cursor.seq) ?: return
        processProjection(projection, freshness)
    }

    private fun handleSubscriptionError(key: ProjectionKey, generation: Long, error: CanonicalError) {
        if (subscriptions[key]?.generation != generation) return
        showMessage(if (error.retriable) "订阅需要恢复，将沿用最近游标" else "订阅被关闭且不可自动恢复")
    }

    private fun handleSubscriptionClosed(key: ProjectionKey, generation: Long, error: CanonicalError?) {
        val active = subscriptions[key] ?: return
        if (active.generation != generation) return
        subscriptions.remove(key)
        runCatching { active.subscription.destroy() }
        if (error?.retriable == true && connected) {
            subscribeKey(key)
            return
        }
        connected = false
        discardSubscriptionHandles()
        markOffline(if (error == null) "连接已关闭" else "订阅关闭，等待重新连接")
    }

    private fun unsubscribeKey(key: ProjectionKey) {
        val active = subscriptions.remove(key) ?: return
        runCatching { active.subscription.unsubscribe() }
        runCatching { active.subscription.destroy() }
    }

    private fun closeSubscriptions(sendUnsubscribe: Boolean) {
        val active = subscriptions.values.toList()
        subscriptions.clear()
        if (sendUnsubscribe) {
            active.forEach { runCatching { it.subscription.unsubscribe() } }
        }
        active.forEach { runCatching { it.subscription.destroy() } }
    }

    private fun discardSubscriptionHandles() {
        closeSubscriptions(sendUnsubscribe = false)
    }

    private fun processProjection(envelope: ProjectionEnvelope, freshness: DataFreshness) {
        when (val payload = envelope.payload) {
            is ProjectionPayload.ProjectIndex -> {
                projectIndexView = payload.view
                projectIndexFreshness = freshness
                val projects = ProjectionMapper.projects(payload.view)
                projectNames.clear()
                projects.forEach { projectNames[it.id] = it.displayName }
                projectRuntimes.clear()
                projectRuntimes.putAll(ProjectionMapper.projectRuntimeIds(payload.view))
                projectBindingRuntimes.clear()
                projectBindingRuntimes.putAll(
                    ProjectionMapper.projectBindingRuntimeIds(payload.view),
                )
                hostRuntimeIds.clear()
                hostRuntimeIds += payload.view.groups.flatMap { it.runtimeIds }.map { it.value }
                renderProjectIndex()
                renderAttention()
                if (connected) reconcileSubscriptions()
            }
            is ProjectionPayload.SessionIndex -> {
                val projectId = payload.view.projectId.value
                sessionViews[projectId] = payload.view
                sessionFreshness[projectId] = freshness
                val (_, raw) = ProjectionMapper.sessions(payload.view)
                sessionsByProject[projectId] = raw
                rebuildSessionIndex()
                if (mutableState.value.selectedProjectId == projectId) renderSelectedSessions()
                renderAttention()
                if (connected) reconcileSubscriptions()
                refreshActionAvailability()
            }
            is ProjectionPayload.Transcript -> {
                val sessionId = payload.view.sessionId.value
                transcriptViews[sessionId] = payload.view
                transcriptFreshness[sessionId] = freshness
                if (mutableState.value.selectedSessionId == sessionId) {
                    renderTranscript(sessionId)
                    renderLive(sessionId)
                }
            }
            is ProjectionPayload.LiveActivity -> {
                val sessionId = payload.view.sessionId.value
                liveViews[sessionId] = payload.view
                liveFreshness[sessionId] = freshness
                if (mutableState.value.selectedSessionId == sessionId) renderLive(sessionId)
            }
            is ProjectionPayload.InputQueue -> {
                val sessionId = payload.view.sessionId.value
                queueViews[sessionId] = payload.view
                queueFreshness[sessionId] = freshness
                if (mutableState.value.selectedSessionId == sessionId) renderQueue(sessionId)
                refreshActionAvailability()
            }
            is ProjectionPayload.AttentionInbox -> {
                attentionItems = payload.view.entries
                pendingActions.retainResolvedAttention(
                    payload.view.entries.mapTo(mutableSetOf()) { it.id.value },
                )
                attentionFreshness = freshness
                renderAttention()
                if (connected) reconcileSubscriptions()
            }
            is ProjectionPayload.RuntimeCapability -> {
                val runtimeId = payload.view.runtimeId.value
                capabilities[runtimeId] = payload.view
                capabilityFreshness[runtimeId] = freshness
                if (mutableState.value.selectedRuntimeId == runtimeId) renderSelectedCapabilities()
                renderAttention()
                refreshActionAvailability()
            }
            is ProjectionPayload.WorkflowBoard -> Unit
        }
    }

    private fun readText(freshness: DataFreshness): (uniffi.kaleido_proto.ContentRef) -> TextResult = { reference ->
        if (freshness != DataFreshness.Live || !connected) {
            TextResult(reference.preview, if (reference.preview == null) "离线缓存不持久化正文" else null)
        } else {
            ephemeralText[reference.contentId.value] ?: try {
                ProjectionMapper.text(client?.readTextContent(reference) ?: error("client unavailable"))
                    .also { ephemeralText[reference.contentId.value] = it }
            } catch (_: MobileClientException) {
                TextResult(null, "正文读取失败")
            } catch (_: RuntimeException) {
                TextResult(null, "正文读取失败")
            }
        }
    }

    private fun renderProjectIndex() {
        val view = projectIndexView ?: return
        val freshness = effectiveFreshness(projectIndexFreshness)
        val mappedHost = ProjectionMapper.host(view.hostId.value, view)
        val host = if (freshness == DataFreshness.CachedOffline) mappedHost.asOffline() else mappedHost
        mutableState.update {
            it.copy(
                hosts = listOf(host),
                projects = PanelState(ProjectionMapper.projects(view), freshness = freshness),
                selectedHostId = view.hostId.value,
            )
        }
        updateOfflineCacheFlag()
    }

    private fun renderSelectedSessions() {
        val projectId = mutableState.value.selectedProjectId ?: return
        val view = sessionViews[projectId] ?: return
        val freshness = effectiveFreshness(sessionFreshness[projectId] ?: DataFreshness.CachedOffline)
        val sections = ProjectionMapper.sessions(view).first.let {
            if (freshness == DataFreshness.CachedOffline) it.asOffline() else it
        }
        mutableState.update { it.copy(sessions = PanelState(sections, freshness = freshness)) }
    }

    private fun renderTranscript(sessionId: String) {
        val view = transcriptViews[sessionId] ?: return
        val freshness = effectiveFreshness(transcriptFreshness[sessionId] ?: DataFreshness.CachedOffline)
        val (mapped, items) = ProjectionMapper.transcript(view, readText(freshness))
        transcriptItems[sessionId] = items
        mutableState.update { it.copy(transcript = PanelState(mapped, freshness = freshness)) }
    }

    private fun renderLive(sessionId: String) {
        val view = liveViews[sessionId] ?: return
        val freshness = effectiveFreshness(liveFreshness[sessionId] ?: DataFreshness.CachedOffline)
        val mapped = ProjectionMapper.liveActivity(
            view,
            transcriptItems[sessionId].orEmpty(),
            readText(freshness),
        )
        mutableState.update { it.copy(liveActivity = PanelState(mapped, freshness = freshness)) }
    }

    private fun renderQueue(sessionId: String) {
        val view = queueViews[sessionId] ?: return
        val freshness = effectiveFreshness(queueFreshness[sessionId] ?: DataFreshness.CachedOffline)
        mutableState.update {
            it.copy(queue = PanelState(ProjectionMapper.queue(view, readText(freshness)), freshness = freshness))
        }
    }

    private fun renderSelectedCapabilities() {
        val runtimeId = mutableState.value.selectedRuntimeId ?: return
        val view = capabilities[runtimeId] ?: return
        val freshness = effectiveFreshness(capabilityFreshness[runtimeId] ?: DataFreshness.CachedOffline)
        mutableState.update {
            it.copy(capabilities = PanelState(ProjectionMapper.capabilities(view), freshness = freshness))
        }
    }

    private fun renderAttention() {
        val freshness = effectiveFreshness(attentionFreshness)
        val mapped = attentionItems.map { item ->
            val session = item.sessionId?.value?.let(sessionsById::get)
            val runtimeId = liveRuntimeId(session?.liveBinding)
                ?: session?.projectBindingId?.value?.let(projectBindingRuntimes::get)
            val capability = runtimeId?.let(capabilities::get)
            val slot = PendingActionSlot.Attention(item.id.value)
            val availability = when {
                pendingActions.isInFlight(slot) -> ActionAvailability.disabled("回答正在提交")
                pendingActions.isResolved(slot) -> ActionAvailability.disabled("回答已送达，等待提醒列表更新")
                freshness != DataFreshness.Live || !connected -> ActionAvailability.disabled("离线缓存不能回答")
                sessionFreshness[item.projectId.value] != DataFreshness.Live ||
                    runtimeId == null || capabilityFreshness[runtimeId] != DataFreshness.Live ->
                    ActionAvailability.disabled("等待实时会话与能力投影确认")
                else -> ProjectionMapper.actionAvailability(
                    mobileAttentionActionAvailability(item, session, capability),
                )
            }
            ProjectionMapper.attention(
                item,
                projectNames[item.projectId.value] ?: item.projectId.value.takeLast(10),
                session?.title,
                availability,
                readText(freshness),
            )
        }
        mutableState.update { it.copy(attention = PanelState(mapped, freshness = freshness)) }
    }

    private fun rebuildSessionIndex() {
        sessionsById.clear()
        sessionsByProject.values.forEach { sessionsById.putAll(it) }
    }

    private fun renderAllSelected() {
        renderProjectIndex()
        renderSelectedSessions()
        mutableState.value.selectedSessionId?.let {
            renderTranscript(it)
            renderLive(it)
            renderQueue(it)
        }
        renderSelectedCapabilities()
        renderAttention()
        refreshActionAvailability()
    }

    private fun refreshActionAvailability() {
        val sessionId = mutableState.value.selectedSessionId
        val session = sessionId?.let(sessionsById::get)
        if (session == null || !connected) {
            disableAllActions(if (connected) "选择一个会话后可用" else "离线状态不能发送命令")
            return
        }
        val projectId = mutableState.value.selectedProjectId
        if (projectId == null || sessionFreshness[projectId] != DataFreshness.Live) {
            disableAllActions("等待实时会话投影确认")
            return
        }
        val runtimeId = liveRuntimeId(session.liveBinding)
            ?: projectBindingRuntimes[session.projectBindingId.value]
        val capability = runtimeId?.let(capabilities::get)
        val queue = queueViews[sessionId]
        val live = liveViews[sessionId]
        mutableState.update {
            it.copy(
                promptAction = if (runtimeId != null && capabilityFreshness[runtimeId] == DataFreshness.Live) {
                    availabilityUnlessInFlight(
                        PendingActionSlot.Prompt,
                        mobileSessionActionAvailability(session, queue, capability, MobileSessionAction.SUBMIT_PROMPT),
                    )
                } else {
                    ActionAvailability.disabled("等待实时能力投影确认")
                },
                enqueueNewTurnAction = if (queueFreshness[sessionId] == DataFreshness.Live) {
                    availabilityUnlessInFlight(
                        PendingActionSlot.EnqueueNewTurn,
                        mobileSessionActionAvailability(session, queue, capability, MobileSessionAction.ENQUEUE_NEW_TURN),
                    )
                } else {
                    ActionAvailability.disabled("等待实时队列投影确认")
                },
                enqueueSteerAction = if (queueFreshness[sessionId] == DataFreshness.Live) {
                    availabilityUnlessInFlight(
                        PendingActionSlot.EnqueueSteer,
                        mobileSessionActionAvailability(session, queue, capability, MobileSessionAction.ENQUEUE_STEER),
                    )
                } else {
                    ActionAvailability.disabled("等待实时队列投影确认")
                },
                resumeAction = if (runtimeId != null && capabilityFreshness[runtimeId] == DataFreshness.Live) {
                    availabilityUnlessInFlight(
                        PendingActionSlot.Resume,
                        mobileSessionActionAvailability(session, queue, capability, MobileSessionAction.RESUME_SESSION),
                    )
                } else {
                    ActionAvailability.disabled("等待运行时恢复能力投影确认")
                },
                interruptAction = if (
                    live?.activeTurnId != null &&
                    runtimeId != null &&
                    capabilityFreshness[runtimeId] == DataFreshness.Live
                ) {
                    availabilityUnlessInFlight(
                        PendingActionSlot.Interrupt,
                        mobileSessionActionAvailability(session, queue, capability, MobileSessionAction.INTERRUPT_TURN),
                    )
                } else {
                    ActionAvailability.disabled("当前没有可中断的活动回合")
                },
            )
        }
    }

    private fun availabilityUnlessInFlight(
        slot: PendingActionSlot,
        value: uniffi.kaleido_core.MobileActionAvailability,
    ): ActionAvailability = if (pendingActions.isInFlight(slot)) {
        ActionAvailability.disabled("命令正在提交")
    } else {
        ProjectionMapper.actionAvailability(value)
    }

    private fun disableAllActions(reason: String) {
        mutableState.update {
            it.copy(
                promptAction = ActionAvailability.disabled(reason),
                enqueueNewTurnAction = ActionAvailability.disabled(reason),
                enqueueSteerAction = ActionAvailability.disabled(reason),
                resumeAction = ActionAvailability.disabled(reason),
                interruptAction = ActionAvailability.disabled(reason),
            )
        }
        renderAttention()
    }

    private fun submitPrompt() {
        val active = client ?: return
        val snapshot = mutableState.value
        val sessionId = snapshot.selectedSessionId ?: return showMessage("请先选择会话")
        if (!snapshot.promptAction.enabled) return showMessage(snapshot.promptAction.disabledReason ?: "当前不可发送")
        val text = snapshot.draft
        if (text.isBlank()) return showMessage("输入内容不能为空")
        val signature = PendingActionSignature(PendingActionKind.Prompt, sessionId, text, null)
        performCommand(active, PendingActionSlot.Prompt, signature) { key ->
            active.submitPromptText(SessionId(sessionId), text, key)
        }.onDefiniteSuccess {
            mutableState.update { state -> if (state.draft == text) state.copy(draft = "") else state }
        }
    }

    private fun resumeSession() {
        val active = client ?: return
        val snapshot = mutableState.value
        val sessionId = snapshot.selectedSessionId ?: return showMessage("请先选择会话")
        if (!snapshot.resumeAction.enabled) {
            return showMessage(snapshot.resumeAction.disabledReason ?: "当前会话不可恢复")
        }
        val signature = PendingActionSignature(PendingActionKind.Resume, sessionId, "", null)
        performCommand(active, PendingActionSlot.Resume, signature) { key ->
            active.resumeSession(SessionId(sessionId), key)
        }
    }

    private fun interruptTurn() {
        val active = client ?: return
        val snapshot = mutableState.value
        val sessionId = snapshot.selectedSessionId ?: return showMessage("请先选择会话")
        val turnId = liveViews[sessionId]?.activeTurnId?.value
            ?: return showMessage("当前没有可中断的活动回合")
        if (!snapshot.interruptAction.enabled) {
            return showMessage(snapshot.interruptAction.disabledReason ?: "当前回合不可中断")
        }
        val signature = PendingActionSignature(PendingActionKind.Interrupt, sessionId, "", turnId)
        performCommand(active, PendingActionSlot.Interrupt, signature) { key ->
            active.interruptTurn(SessionId(sessionId), TurnId(turnId), key)
        }
    }

    private fun enqueue(intent: QueueIntentUi) {
        val active = client ?: return
        val snapshot = mutableState.value
        val sessionId = snapshot.selectedSessionId ?: return showMessage("请先选择会话")
        val availability = if (intent == QueueIntentUi.NewTurn) snapshot.enqueueNewTurnAction else snapshot.enqueueSteerAction
        if (!availability.enabled) return showMessage(availability.disabledReason ?: "队列不可写")
        val text = snapshot.draft
        if (text.isBlank()) return showMessage("输入内容不能为空")
        val slot = if (intent == QueueIntentUi.NewTurn) PendingActionSlot.EnqueueNewTurn else PendingActionSlot.EnqueueSteer
        val kind = if (intent == QueueIntentUi.NewTurn) PendingActionKind.EnqueueNewTurn else PendingActionKind.EnqueueSteer
        val signature = PendingActionSignature(kind, sessionId, text, null)
        performCommand(active, slot, signature) { key ->
            active.enqueueText(
                SessionId(sessionId),
                text,
                if (intent == QueueIntentUi.NewTurn) QueueIntent.NEW_TURN else QueueIntent.STEER_ACTIVE_TURN,
                key,
            )
        }.onDefiniteSuccess {
            mutableState.update { state -> if (state.draft == text) state.copy(draft = "") else state }
        }
    }

    private fun respondAttention(action: UiAction.RespondAttention) {
        val active = client ?: return
        val item = attentionItems.firstOrNull { it.id.value == action.attentionId }
            ?: return showMessage("提醒已变化，请刷新")
        val slot = PendingActionSlot.Attention(action.attentionId)
        val signature = PendingActionSignature(
            PendingActionKind.Attention,
            action.attentionId,
            action.freeForm.orEmpty(),
            action.optionId,
        )
        performCommand(active, slot, signature, retainDefiniteSuccess = true) { key ->
            active.respondAttentionText(
                item,
                action.optionId,
                action.freeForm?.takeIf(String::isNotBlank),
                key,
            )
        }.onDefiniteSuccess {
            mutableState.update { state ->
                state.copy(attentionDrafts = state.attentionDrafts - action.attentionId)
            }
        }
    }

    private fun respondQuestion(action: UiAction.RespondQuestion) {
        val active = client ?: return
        val item = attentionItems.firstOrNull { it.id.value == action.attentionId }
            ?: return showMessage("提醒已变化，请刷新")
        val slot = PendingActionSlot.Attention(action.attentionId)
        val signature = PendingActionSignature(
            PendingActionKind.Attention,
            action.attentionId,
            action.answers.joinToString("|") { answer ->
                "${answer.questionKey}:${answer.optionIds.joinToString(",")}:${answer.freeForm.orEmpty()}"
            },
            null,
        )
        performCommand(active, slot, signature, retainDefiniteSuccess = true) { key ->
            active.respondQuestionText(
                item,
                action.answers.map { answer ->
                    MobileQuestionAnswer(
                        questionKey = answer.questionKey,
                        optionIds = answer.optionIds,
                        freeForm = answer.freeForm,
                    )
                },
                key,
            )
        }
    }

    private fun performCommand(
        active: MobileClient,
        slot: PendingActionSlot,
        signature: PendingActionSignature,
        retainDefiniteSuccess: Boolean = false,
        block: (String) -> CommandAck,
    ): CommandAttempt {
        val pending = try {
            pendingActions.begin(slot, signature) { active.createActionId() }
        } catch (_: MobileClientException) {
            showMessage("无法创建安全的命令幂等键")
            return CommandAttempt.Uncertain
        } catch (_: RuntimeException) {
            showMessage("无法创建安全的命令幂等键")
            return CommandAttempt.Uncertain
        }
        if (pending == null) {
            showMessage(
                if (pendingActions.isResolved(slot)) "操作已送达，等待最新投影"
                else "同一操作正在提交",
            )
            return CommandAttempt.InFlight
        }
        refreshActionAvailability()
        renderAttention()
        val attempt = try {
            val ack = block(pending.idempotencyKey)
            showMessage(ackMessage(ack))
            if (ack.outcome.isDefiniteSuccess()) CommandAttempt.DefiniteSuccess else CommandAttempt.DefiniteRejection
        } catch (_: MobileClientException.RemoteRejected) {
            showMessage("Broker 明确拒绝了命令")
            CommandAttempt.DefiniteRejection
        } catch (_: MobileClientException) {
            showMessage("命令结果未知；重试会沿用同一幂等键")
            CommandAttempt.Uncertain
        } catch (_: RuntimeException) {
            showMessage("命令结果未知；重试会沿用同一幂等键")
            CommandAttempt.Uncertain
        }
        pendingActions.complete(slot, attempt.completion(), retainDefiniteSuccess)
        refreshActionAvailability()
        renderAttention()
        return attempt
    }

    private fun ackMessage(ack: CommandAck): String = when (ack.outcome) {
        is CommandOutcome.AcceptedLocally -> "命令已由 Broker 持久记录，尚未证明 Runtime 接受"
        is CommandOutcome.AcceptedByRuntime -> "Runtime 已接受命令"
        is CommandOutcome.Enqueued -> "输入已排队"
        is CommandOutcome.Rejected -> "命令被 Broker 拒绝；输入已保留"
        is CommandOutcome.Duplicate -> "重复请求已安全去重"
    }

    private fun markOffline(reason: String) {
        connected = false
        ephemeralText.clear()
        projectIndexFreshness = DataFreshness.CachedOffline
        attentionFreshness = DataFreshness.CachedOffline
        sessionFreshness.keys.toList().forEach { sessionFreshness[it] = DataFreshness.CachedOffline }
        transcriptFreshness.keys.toList().forEach { transcriptFreshness[it] = DataFreshness.CachedOffline }
        liveFreshness.keys.toList().forEach { liveFreshness[it] = DataFreshness.CachedOffline }
        queueFreshness.keys.toList().forEach { queueFreshness[it] = DataFreshness.CachedOffline }
        capabilityFreshness.keys.toList().forEach { capabilityFreshness[it] = DataFreshness.CachedOffline }
        val current = mutableState.value
        mutableState.update {
            it.copy(
                connection = ConnectionUiState.Offline(
                    hostName = current.selectedHostId?.let(::hostLabel),
                    reason = reason,
                    cachedDataAvailable = projectIndexView != null,
                ),
            )
        }
        renderAllSelected()
        disableAllActions("离线状态不能发送命令")
    }

    private fun updateOfflineCacheFlag() {
        val connection = mutableState.value.connection
        if (connection is ConnectionUiState.Offline) {
            mutableState.update {
                it.copy(connection = connection.copy(cachedDataAvailable = projectIndexView != null))
            }
        }
    }

    private fun showMessage(text: String) {
        mutableState.update { it.copy(message = UiMessage(messageIds.getAndIncrement(), text)) }
    }

    private fun effectiveFreshness(recorded: DataFreshness): DataFreshness =
        if (connected && recorded == DataFreshness.Live) DataFreshness.Live else DataFreshness.CachedOffline

    override fun close() {
        if (!closing.compareAndSet(false, true)) return
        scope.launch {
            connected = false
            closeSubscriptions(sendUnsubscribe = true)
            val active = client
            runCatching { active?.disconnect() }
            workerStarted = false
            runCatching { active?.destroy() }
            client = null
            ephemeralText.clear()
            scope.cancel()
        }
    }

    private fun hostLabel(hostId: String): String = "PC ${hostId.takeLast(8)}"

    private fun liveRuntimeId(binding: LiveBinding?): String? = when (binding) {
        is LiveBinding.Controlling -> binding.runtimeId.value
        is LiveBinding.Observing -> binding.runtimeId.value
        is LiveBinding.Blocked, is LiveBinding.NotBound, null -> null
    }
}

private data class ActiveSubscription(
    val generation: Long,
    val subscription: ProjectionSubscription,
)

private enum class ProjectionCursorObservation { Advanced, Equal, Stale }

/**
 * Highest cursor already rendered or observed for each projection key.
 *
 * Rust persists a projection before invoking its callback. A cache read can therefore observe a
 * newer cursor while older callbacks are still queued on the Kotlin IO actor. Recording cache
 * cursors here prevents those queued callbacks from rolling product state backwards; an equal
 * live callback remains admissible because it is the evidence that turns that cached view live.
 */
internal class ProjectionCursorHighWater<K> {
    private val cursors = mutableMapOf<K, ULong>()

    fun acceptCached(key: K, cursor: ULong): Boolean =
        observe(key, cursor) == ProjectionCursorObservation.Advanced

    fun acceptLive(key: K, cursor: ULong): Boolean =
        observe(key, cursor) != ProjectionCursorObservation.Stale

    fun synchronizedFreshness(key: K, cursor: ULong): DataFreshness? =
        if (acceptLive(key, cursor)) DataFreshness.Live else null

    private fun observe(key: K, cursor: ULong): ProjectionCursorObservation {
        val previous = cursors[key]
        return when {
            previous == null || cursor > previous -> {
                cursors[key] = cursor
                ProjectionCursorObservation.Advanced
            }
            cursor == previous -> ProjectionCursorObservation.Equal
            else -> ProjectionCursorObservation.Stale
        }
    }
}

internal enum class PendingActionKind { Prompt, EnqueueNewTurn, EnqueueSteer, Resume, Interrupt, Attention }

internal sealed interface PendingActionSlot {
    data object Prompt : PendingActionSlot
    data object EnqueueNewTurn : PendingActionSlot
    data object EnqueueSteer : PendingActionSlot
    data object Resume : PendingActionSlot
    data object Interrupt : PendingActionSlot
    data class Attention(val attentionId: String) : PendingActionSlot
}

internal data class PendingActionSignature(
    val kind: PendingActionKind,
    val targetId: String,
    val text: String,
    val optionId: String?,
)

internal data class PendingMobileAction(
    val signature: PendingActionSignature,
    val idempotencyKey: String,
    val state: PendingActionState,
)

internal enum class PendingActionState { InFlight, Retriable, Resolved }

internal enum class CommandCompletion { DefiniteSuccess, DefiniteRejection, Uncertain }

internal class PendingActionTracker {
    private val actions = mutableMapOf<PendingActionSlot, PendingMobileAction>()

    fun begin(
        slot: PendingActionSlot,
        signature: PendingActionSignature,
        createKey: () -> String,
    ): PendingMobileAction? {
        val existing = actions[slot]
        if (existing?.state == PendingActionState.InFlight || existing?.state == PendingActionState.Resolved) {
            return null
        }
        val pending = if (existing != null && existing.signature == signature) {
            existing.copy(state = PendingActionState.InFlight)
        } else {
            PendingMobileAction(signature, createKey(), state = PendingActionState.InFlight)
        }
        actions[slot] = pending
        return pending
    }

    fun complete(
        slot: PendingActionSlot,
        completion: CommandCompletion,
        retainDefiniteSuccess: Boolean = false,
    ) {
        when (completion) {
            CommandCompletion.DefiniteSuccess -> if (retainDefiniteSuccess) {
                actions[slot]?.let { actions[slot] = it.copy(state = PendingActionState.Resolved) }
            } else {
                actions.remove(slot)
            }
            CommandCompletion.DefiniteRejection -> actions.remove(slot)
            CommandCompletion.Uncertain -> actions[slot]?.let {
                actions[slot] = it.copy(state = PendingActionState.Retriable)
            }
        }
    }

    fun retainResolvedAttention(activeIds: Set<String>) {
        actions.keys.removeAll { slot ->
            slot is PendingActionSlot.Attention &&
                slot.attentionId !in activeIds &&
                actions[slot]?.state == PendingActionState.Resolved
        }
    }

    fun isInFlight(slot: PendingActionSlot): Boolean =
        actions[slot]?.state == PendingActionState.InFlight

    fun isResolved(slot: PendingActionSlot): Boolean =
        actions[slot]?.state == PendingActionState.Resolved

    fun pending(slot: PendingActionSlot): PendingMobileAction? = actions[slot]
}

internal sealed interface CommandAttempt {
    data object DefiniteSuccess : CommandAttempt
    data object DefiniteRejection : CommandAttempt
    data object Uncertain : CommandAttempt
    data object InFlight : CommandAttempt

    fun completion(): CommandCompletion = when (this) {
        DefiniteSuccess -> CommandCompletion.DefiniteSuccess
        DefiniteRejection -> CommandCompletion.DefiniteRejection
        Uncertain, InFlight -> CommandCompletion.Uncertain
    }

    fun onDefiniteSuccess(block: () -> Unit) {
        if (this == DefiniteSuccess) block()
    }
}

internal fun CommandOutcome.isDefiniteSuccess(): Boolean = when (this) {
    is CommandOutcome.AcceptedLocally,
    is CommandOutcome.AcceptedByRuntime,
    is CommandOutcome.Enqueued,
    is CommandOutcome.Duplicate,
    -> true
    is CommandOutcome.Rejected -> false
}

private fun HostUi.asOffline(): HostUi = copy(
    reachability = ReachabilityUi.Offline,
    lastSeenLabel = "离线缓存",
)

private fun SessionSectionsUi.asOffline(): SessionSectionsUi = copy(
    active = active.map(SessionUi::asOffline),
    history = history.map(SessionUi::asOffline),
    archived = archived.map(SessionUi::asOffline),
)

private fun SessionUi.asOffline(): SessionUi = copy(
    liveBindingLabel = "离线缓存",
    isLive = false,
)
