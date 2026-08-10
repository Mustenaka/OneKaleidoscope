package com.onekaleidoscope.integration

import android.os.Bundle
import android.os.SystemClock
import android.util.Base64
import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import com.onekaleidoscope.data.MobileRepository
import com.onekaleidoscope.platform.AndroidCoreStorage
import com.onekaleidoscope.platform.AndroidDeviceSigner
import com.onekaleidoscope.platform.AndroidSecureCredentialVault
import com.onekaleidoscope.ui.AppUiState
import com.onekaleidoscope.ui.AttentionSubjectUi
import com.onekaleidoscope.ui.ConnectionUiState
import com.onekaleidoscope.ui.DataFreshness
import com.onekaleidoscope.ui.DecisionToneUi
import com.onekaleidoscope.ui.QueueIntentUi
import com.onekaleidoscope.ui.SessionUi
import com.onekaleidoscope.ui.UiAction
import java.nio.charset.StandardCharsets
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import uniffi.kaleido_core.MobileClient
import uniffi.kaleido_core.MobileClientException
import uniffi.kaleido_proto.ProjectionKey

/**
 * Externally orchestrated, real-product LAN acceptance phases. No phase starts or fakes hostd.
 *
 * The harness must drive separate instrumentation processes:
 *
 *  1. clear app data, then `-e lanPhase wrong-pin -e pairingUri <uri>`;
 *  2. clear app data, then `-e lanPhase seed -e pairingUri <fresh-uri>`;
 *  3. `adb shell am force-stop com.onekaleidoscope`;
 *  4. run `-e lanPhase resume` without clearing app data;
 *  5. revoke the reported DeviceId in the real hostd, force-stop again, then run
 *     `-e lanPhase revoked`.
 *
 * Splitting the phases is intentional: force-stopping the target package also kills an
 * in-process instrumentation runner, so a same-process repository recreation is not presented as
 * cold-start evidence. The normal Android CI run supplies no `lanPhase` and skips only this
 * host-dependent class; [NativeUniFfiSmokeTest] remains unconditional.
 */
@RunWith(AndroidJUnit4::class)
class RealLanBridgeTest {
    private val instrumentation = InstrumentationRegistry.getInstrumentation()
    private val context = instrumentation.targetContext

    @Test
    fun runExternallySelectedRealLanPhase() {
        val arguments = InstrumentationRegistry.getArguments()
        val phase = arguments.getString(ARG_PHASE)
        assumeTrue("lanPhase is supplied only by the real hostd acceptance harness", !phase.isNullOrBlank())

        when (phase) {
            PHASE_WRONG_PIN -> rejectWrongPin(requirePairingUri(arguments))
            PHASE_SEED -> pairRenderAndSubmit(
                requirePairingUri(arguments),
                arguments.getString(ARG_REQUIRE_ATTENTION)?.toBooleanStrictOrNull() == true,
            )
            PHASE_BACKGROUND -> resumeAfterExternalBackground()
            PHASE_RESUME -> resumeAfterExternalForceStop()
            PHASE_REVOKED -> rejectRevokedCredential()
            else -> throw AssertionError("unknown lanPhase=$phase")
        }
    }

    private fun rejectWrongPin(pairingUri: String) {
        val vault = AndroidSecureCredentialVault(context)
        assertNull("wrong-pin phase requires the harness to clear app data first", vault.loadPairedHost())
        val client = MobileClient.newWithSecureVault(
            AndroidCoreStorage.projectionCacheDirectory(context).absolutePath,
            AndroidDeviceSigner(),
            vault,
        )
        try {
            assertThrows(MobileClientException.Authentication::class.java) {
                client.pair(withDifferentValidPin(pairingUri), DEVICE_LABEL)
            }
            assertNull("failed TLS pin verification persisted a paired-host envelope", vault.loadPairedHost())
            reportEvidence(outcome = "wrong-pin-authentication", deviceId = null, cursor = null)
        } finally {
            client.destroy()
        }
    }

    private fun pairRenderAndSubmit(pairingUri: String, requireAttention: Boolean) {
        assertNull(
            "seed phase requires the harness to clear app data first",
            AndroidSecureCredentialVault(context).loadPairedHost(),
        )
        val repository = MobileRepository(context)
        try {
            awaitState(repository, "unpaired initialization") { it.connection == ConnectionUiState.Unpaired }
            repository.dispatch(UiAction.SubmitPairingQr(pairingUri))
            val projectState = awaitState(repository, "live ProjectIndex") {
                it.connection is ConnectionUiState.Live && !it.projects.value.isNullOrEmpty()
            }
            val project = requireNotNull(projectState.projects.value).first()
            repository.dispatch(UiAction.SelectProject(project.id))
            val sessionState = awaitState(repository, "live SessionIndex") {
                selectedSessions(it).isNotEmpty()
            }
            val session = selectedSessions(sessionState).first()
            repository.dispatch(UiAction.SelectSession(session.id))
            val allProjections = awaitState(repository, "seven product projections") { state ->
                state.projects.value != null &&
                    state.projects.freshness == DataFreshness.Live &&
                    state.sessions.value != null &&
                    state.sessions.freshness == DataFreshness.Live &&
                    state.transcript.value != null &&
                    state.transcript.freshness == DataFreshness.Live &&
                    state.liveActivity.value != null &&
                    state.liveActivity.freshness == DataFreshness.Live &&
                    state.queue.value != null &&
                    state.queue.freshness == DataFreshness.Live &&
                    state.attention.value != null &&
                    state.attention.freshness == DataFreshness.Live &&
                    state.capabilities.value != null &&
                    state.capabilities.freshness == DataFreshness.Live
            }
            assertSevenProjectionState(allProjections, DataFreshness.Live)
            assertEquals(session.id, allProjections.transcript.value?.sessionId)
            assertEquals(session.id, allProjections.liveActivity.value?.sessionId)
            assertEquals(session.id, allProjections.queue.value?.sessionId)

            repository.dispatch(UiAction.SelectProject(project.id))
            awaitState(repository, "selection isolation clears session panels") {
                it.selectedSessionId == null &&
                    it.transcript.value == null &&
                    it.liveActivity.value == null &&
                    it.queue.value == null
            }
            repository.dispatch(UiAction.SelectSession(session.id))
            val reselected = awaitState(repository, "reselected session projections") {
                it.transcript.value?.sessionId == session.id &&
                    it.liveActivity.value?.sessionId == session.id &&
                    it.queue.value?.sessionId == session.id
            }

            val prompt = if (requireAttention) {
                "Use the file-editing tool to replace the complete contents of editable.txt " +
                    "with exactly KALEIDO PHYSICAL APPROVAL PROBE. Do not run a shell command, " +
                    "do not create or touch any other file, and do not access any path outside " +
                    "the current project."
            } else {
                "T-108 Android product-path command ${System.currentTimeMillis()}"
            }
            val previousMessageId = reselected.message?.id
            repository.dispatch(UiAction.UpdateDraft(prompt))
            val actionState = awaitState(repository, "draft update") { it.draft == prompt }
            val commandKind = when {
                actionState.promptAction.enabled -> {
                    repository.dispatch(UiAction.SubmitPrompt)
                    "submit-prompt"
                }
                actionState.enqueueNewTurnAction.enabled -> {
                    repository.dispatch(UiAction.EnqueueInput(QueueIntentUi.NewTurn))
                    "enqueue-new-turn"
                }
                else -> throw AssertionError(
                    "real session exposes neither prompt nor enqueue: " +
                        "prompt=${actionState.promptAction.disabledReason}; " +
                        "enqueue=${actionState.enqueueNewTurnAction.disabledReason}",
                )
            }
            val commandState = awaitState(repository, "broker command acknowledgement") {
                it.draft.isEmpty() && it.message?.id != null && it.message.id != previousMessageId
            }
            assertTrue(
                "command was not accepted on the real repository path: ${commandState.message?.text}",
                commandState.message?.text in ACCEPTED_COMMAND_MESSAGES,
            )
            val attentionOutcome = if (requireAttention) {
                respondToRealApproval(repository)
                "-attention-declined"
            } else {
                ""
            }

            val stableProjectIds = requireNotNull(commandState.projects.value).map { it.id }
            val stableSessionIds = selectedSessions(commandState).map { it.id }
            repository.dispatch(UiAction.DisconnectHost(requireNotNull(commandState.selectedHostId)))
            val disconnected = awaitState(repository, "repository disconnect") {
                it.connection is ConnectionUiState.Offline &&
                    it.projects.freshness == DataFreshness.CachedOffline
            }
            assertFalse(disconnected.promptAction.enabled)
            repository.dispatch(UiAction.RetryConnection)
            val reconnected = awaitState(repository, "repository reconnect") {
                it.connection is ConnectionUiState.Live &&
                    it.projects.freshness == DataFreshness.Live &&
                    it.transcript.freshness == DataFreshness.Live
            }
            assertEquals(stableProjectIds, requireNotNull(reconnected.projects.value).map { it.id })
            assertEquals(stableSessionIds, selectedSessions(reconnected).map { it.id })

            val evidence = readStoredEvidence()
            reportEvidence(
                outcome = "seed-seven-projections-$commandKind$attentionOutcome",
                deviceId = evidence.deviceId,
                cursor = evidence.cursor,
            )
        } finally {
            repository.close()
        }
    }

    private fun respondToRealApproval(repository: MobileRepository) {
        val attentionState = awaitState(repository, "real runtime approval") { state ->
            state.attention.value.orEmpty().any { item -> item.responseAvailability.enabled }
        }
        val attention = requireNotNull(
            attentionState.attention.value.orEmpty().firstOrNull { it.responseAvailability.enabled },
        )
        val approval = attention.subject as? AttentionSubjectUi.Approval
            ?: throw AssertionError("physical gate expected a real approval, not another attention kind")
        val decline = approval.options.firstOrNull { it.tone == DecisionToneUi.Destructive }
            ?: throw AssertionError("real approval exposes no destructive decline option")
        val previousMessageId = attentionState.message?.id
        repository.dispatch(UiAction.RespondAttention(attention.id, decline.id, null))
        val acknowledged = awaitState(repository, "approval response acknowledgement") { state ->
            state.message?.id != null &&
                state.message.id != previousMessageId &&
                state.message.text in ACCEPTED_COMMAND_MESSAGES
        }
        assertTrue(acknowledged.message?.text in ACCEPTED_COMMAND_MESSAGES)
        awaitState(repository, "answered approval leaves the inbox") { state ->
            state.attention.value.orEmpty().none { it.id == attention.id }
        }
    }

    private fun resumeAfterExternalBackground() {
        val repository = MobileRepository(context)
        try {
            val initial = awaitState(repository, "background credential and project cache") { state ->
                !state.projects.value.isNullOrEmpty() &&
                    (state.connection is ConnectionUiState.Live ||
                        state.connection is ConnectionUiState.Offline)
            }
            if (initial.connection is ConnectionUiState.Offline) {
                repository.dispatch(UiAction.RetryConnection)
            }
            val projectState = awaitState(repository, "background credential reconnect") { state ->
                state.connection is ConnectionUiState.Live && !state.projects.value.isNullOrEmpty()
            }
            val project = requireNotNull(projectState.projects.value).first()
            repository.dispatch(UiAction.SelectProject(project.id))
            val sessionState = awaitState(repository, "background SessionIndex") {
                selectedSessions(it).isNotEmpty()
            }
            val session = selectedSessions(sessionState).first()
            repository.dispatch(UiAction.SelectSession(session.id))
            val live = awaitState(repository, "seven live projections after OEM background") {
                allProjectionPanelsHaveFreshness(it, DataFreshness.Live)
            }
            assertSevenProjectionState(live, DataFreshness.Live)
            val evidence = readStoredEvidence()
            reportEvidence("oem-background-resumed", evidence.deviceId, evidence.cursor)
        } finally {
            repository.close()
        }
    }

    private fun resumeAfterExternalForceStop() {
        val repository = MobileRepository(context)
        try {
            val offline = awaitState(repository, "offline cache after external force-stop") {
                val connection = it.connection
                connection is ConnectionUiState.Offline &&
                    connection.cachedDataAvailable &&
                    !it.projects.value.isNullOrEmpty()
            }
            assertEquals(DataFreshness.CachedOffline, offline.projects.freshness)
            val project = requireNotNull(offline.projects.value).first()
            repository.dispatch(UiAction.SelectProject(project.id))
            val cachedSessions = awaitState(repository, "cached SessionIndex") {
                selectedSessions(it).isNotEmpty() && it.sessions.freshness == DataFreshness.CachedOffline
            }
            val session = selectedSessions(cachedSessions).first()
            repository.dispatch(UiAction.SelectSession(session.id))
            val cached = awaitState(repository, "seven cached product projections") { state ->
                state.transcript.value != null &&
                    state.liveActivity.value != null &&
                    state.queue.value != null &&
                    state.attention.value != null &&
                    state.capabilities.value != null
            }
            assertSevenProjectionState(cached, DataFreshness.CachedOffline)
            assertFalse(cached.promptAction.enabled)
            assertFalse(cached.enqueueNewTurnAction.enabled)
            val resumeFrom = readStoredEvidence()

            val stableProjects = requireNotNull(cached.projects.value).map { it.id }
            val stableSessions = selectedSessions(cached).map { it.id }
            repository.dispatch(UiAction.RetryConnection)
            val live = awaitState(repository, "cursor reconnect") {
                it.connection is ConnectionUiState.Live &&
                    it.projects.value?.map { project -> project.id } == stableProjects &&
                    selectedSessions(it).map { session -> session.id } == stableSessions &&
                    allProjectionPanelsHaveFreshness(it, DataFreshness.Live)
            }
            assertEquals(stableProjects, requireNotNull(live.projects.value).map { it.id })
            assertEquals(stableSessions, selectedSessions(live).map { it.id })
            assertEquals(stableProjects.distinct(), stableProjects)
            assertEquals(stableSessions.distinct(), stableSessions)
            assertSevenProjectionState(live, DataFreshness.Live)
            val evidence = readStoredEvidence()
            reportEvidence(
                "force-stop-cache-cursor-resumed",
                evidence.deviceId,
                evidence.cursor,
                resumeFrom.cursor,
            )
        } finally {
            repository.close()
        }
    }

    private fun rejectRevokedCredential() {
        val repository = MobileRepository(context)
        try {
            awaitState(repository, "persisted credential before revoke check") {
                it.connection is ConnectionUiState.Offline
            }
            repository.dispatch(UiAction.RetryConnection)
            val rejected = awaitState(repository, "revoked credential Authentication") {
                it.connection is ConnectionUiState.Revoked
            }
            assertTrue((rejected.connection as ConnectionUiState.Revoked).reason.isNotBlank())
            val evidence = readStoredEvidence()
            reportEvidence("revoked-authentication", evidence.deviceId, evidence.cursor)
        } finally {
            repository.close()
        }
    }

    private fun assertSevenProjectionState(state: AppUiState, freshness: DataFreshness) {
        assertNotNull(state.projects.value)
        assertNotNull(state.sessions.value)
        assertNotNull(state.transcript.value)
        assertNotNull(state.liveActivity.value)
        assertNotNull(state.queue.value)
        assertNotNull(state.attention.value)
        assertNotNull(state.capabilities.value)
        assertEquals(freshness, state.projects.freshness)
        assertEquals(freshness, state.sessions.freshness)
        assertEquals(freshness, state.transcript.freshness)
        assertEquals(freshness, state.liveActivity.freshness)
        assertEquals(freshness, state.queue.freshness)
        assertEquals(freshness, state.attention.freshness)
        assertEquals(freshness, state.capabilities.freshness)
    }

    private fun allProjectionPanelsHaveFreshness(
        state: AppUiState,
        freshness: DataFreshness,
    ): Boolean = listOf(
        state.projects,
        state.sessions,
        state.transcript,
        state.liveActivity,
        state.queue,
        state.attention,
        state.capabilities,
    ).all { it.value != null && it.freshness == freshness }

    private fun awaitState(
        repository: MobileRepository,
        description: String,
        timeoutMs: Long = STATE_TIMEOUT_MS,
        predicate: (AppUiState) -> Boolean,
    ): AppUiState {
        val deadline = SystemClock.elapsedRealtime() + timeoutMs
        var latest = repository.state.value
        while (SystemClock.elapsedRealtime() < deadline) {
            latest = repository.state.value
            if (predicate(latest)) return latest
            SystemClock.sleep(POLL_INTERVAL_MS)
        }
        throw AssertionError("timed out waiting for $description; ${safeStateSummary(latest)}")
    }

    private fun safeStateSummary(state: AppUiState): String = buildString {
        append("connection=").append(state.connection.javaClass.simpleName)
        append(", selection=").append(state.selectedProjectId != null).append('/')
            .append(state.selectedSessionId != null).append('/')
            .append(state.selectedRuntimeId != null)
        append(", panels=")
        listOf(
            state.projects,
            state.sessions,
            state.transcript,
            state.liveActivity,
            state.queue,
            state.attention,
            state.capabilities,
        ).joinTo(this, separator = "|") { panel ->
            "${panel.freshness}:${panel.loading}:${panel.value != null}:${panel.error != null}"
        }
    }

    private fun selectedSessions(state: AppUiState): List<SessionUi> = state.sessions.value?.let {
        it.active + it.history + it.archived
    }.orEmpty()

    private fun readStoredEvidence(): StoredEvidence {
        val client = MobileClient.newWithSecureVault(
            AndroidCoreStorage.projectionCacheDirectory(context).absolutePath,
            AndroidDeviceSigner(),
            AndroidSecureCredentialVault(context),
        )
        return try {
            val paired = requireNotNull(client.pairedHostInfo())
            val cursor = client.cachedProjection(ProjectionKey.ProjectIndex(paired.hostId))?.cursor?.seq
            StoredEvidence(paired.deviceId.value, cursor?.toString())
        } finally {
            client.destroy()
        }
    }

    private fun reportEvidence(
        outcome: String,
        deviceId: String?,
        cursor: String?,
        resumeFromCursor: String? = null,
    ) {
        instrumentation.sendStatus(
            STATUS_EVIDENCE,
            Bundle().apply {
                putString("onekaleidoscope.outcome", outcome)
                putString("onekaleidoscope.deviceId", deviceId)
                putString("onekaleidoscope.cursor", cursor)
                putString("onekaleidoscope.resumeFromCursor", resumeFromCursor)
                putString("onekaleidoscope.externalForceStopRequired", "true")
            },
        )
    }

    private fun requirePairingUri(arguments: Bundle): String =
        arguments.getString(ARG_PAIRING_URI)?.takeIf(String::isNotBlank)
            ?: throw AssertionError("$ARG_PAIRING_URI is required for this phase")

    private fun withDifferentValidPin(uri: String): String {
        require(uri.startsWith(PAIR_URI_PREFIX))
        val encoded = uri.removePrefix(PAIR_URI_PREFIX)
        val wire = JSONObject(
            Base64.decode(encoded, Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP)
                .toString(StandardCharsets.UTF_8),
        )
        val originalPin = wire.getString("host_public_key_pin")
        val zeroPin = pin(ByteArray(PIN_BYTES))
        val replacement = if (originalPin == zeroPin) pin(ByteArray(PIN_BYTES) { 1 }) else zeroPin
        val canonical = JSONObject()
            .put("version", wire.getLong("version"))
            .put("host_id", wire.getString("host_id"))
            .put("endpoint", wire.getString("endpoint"))
            .put("host_public_key_pin", replacement)
            .put("secret", wire.getString("secret"))
            .put("expires_at_ms", wire.getLong("expires_at_ms"))
            .toString()
            .toByteArray(StandardCharsets.UTF_8)
        return PAIR_URI_PREFIX + Base64.encodeToString(
            canonical,
            Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP,
        )
    }

    private fun pin(digest: ByteArray): String =
        "sha256:" + Base64.encodeToString(
            digest,
            Base64.URL_SAFE or Base64.NO_PADDING or Base64.NO_WRAP,
        )

    private data class StoredEvidence(val deviceId: String, val cursor: String?)

    companion object {
        private const val ARG_PHASE = "lanPhase"
        private const val ARG_PAIRING_URI = "pairingUri"
        private const val ARG_REQUIRE_ATTENTION = "requireAttention"
        private const val PHASE_WRONG_PIN = "wrong-pin"
        private const val PHASE_SEED = "seed"
        private const val PHASE_BACKGROUND = "background"
        private const val PHASE_RESUME = "resume"
        private const val PHASE_REVOKED = "revoked"
        private const val PAIR_URI_PREFIX = "onekaleidoscope://pair/v1?data="
        private const val DEVICE_LABEL = "T-108 Android acceptance"
        private const val PIN_BYTES = 32
        private const val STATUS_EVIDENCE = 108
        private const val STATE_TIMEOUT_MS = 45_000L
        private const val POLL_INTERVAL_MS = 50L
        private val ACCEPTED_COMMAND_MESSAGES = setOf(
            "命令已由 Broker 持久记录，尚未证明 Runtime 接受",
            "Runtime 已接受命令",
            "输入已排队",
            "重复请求已安全去重",
        )
    }
}
