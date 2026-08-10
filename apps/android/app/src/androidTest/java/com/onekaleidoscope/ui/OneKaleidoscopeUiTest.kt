package com.onekaleidoscope.ui

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasClickAction
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.StateRestorationTester
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.unit.Density
import java.util.concurrent.CopyOnWriteArrayList
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test

class OneKaleidoscopeUiTest {
    @get:Rule
    val compose = createComposeRule()

    @Test
    fun offlineProjectionIsExplicitlyMarkedAndCommandsExplainWhyTheyAreDisabled() {
        val reason = "离线状态不能发送命令"
        compose.setContent {
            OneKaleidoscopeApp(
                state = verticalSliceState().copy(
                    connection = ConnectionUiState.Offline("Workstation", "连接中断", true),
                    projects = verticalSliceState().projects.copy(freshness = DataFreshness.CachedOffline),
                    promptAction = ActionAvailability.disabled(reason),
                ),
                onAction = {},
            )
        }

        compose.onNode(hasText("项目") and hasClickAction()).performClick()
        compose.onNodeWithContentDescription("离线缓存内容，可能不是最新状态").assertIsDisplayed()

        navigateToSession()
        compose.onNodeWithText("发送新回合").assertIsNotEnabled()
        compose.onNodeWithText(reason).assertIsDisplayed()
    }

    @Test
    fun projectAndSessionSelectionNavigateToPendingQueueWithoutClaimingDelivery() {
        val actions = CopyOnWriteArrayList<UiAction>()
        compose.setContent {
            OneKaleidoscopeApp(state = verticalSliceState(), onAction = actions::add)
        }

        navigateToSession()
        compose.onNodeWithText("队列").performClick()

        compose.onNodeWithText("等待中").assertIsDisplayed()
        compose.onNodeWithText("已作为新回合送达").assertDoesNotExist()
        compose.onNodeWithText("已确认引导").assertDoesNotExist()
        assertTrue(actions.contains(UiAction.SelectProject(PROJECT_ID)))
        assertTrue(actions.contains(UiAction.SelectSession(SESSION_ID)))
        assertTrue(actions.contains(UiAction.Navigate(AppDestination.Sessions)))
        assertTrue(actions.contains(UiAction.Navigate(AppDestination.Session)))
    }

    @Test
    fun attentionUsesRuntimeOptionsAndOnlyOffersFreeFormWhenAllowed() {
        val actions = CopyOnWriteArrayList<UiAction>()
        var state by mutableStateOf(
            verticalSliceState().copy(
                attention = PanelState(listOf(questionAttention(freeFormAllowed = false))),
            ),
        )
        compose.setContent {
            OneKaleidoscopeApp(
                state = state,
                onAction = { action ->
                    actions += action
                    if (action is UiAction.UpdateAttentionDraft) {
                        state = state.copy(
                            attentionDrafts = state.attentionDrafts + (action.attentionId to action.value),
                        )
                    }
                },
            )
        }

        compose.onNode(hasText("待处理") and hasClickAction()).performClick()
        compose.onNodeWithText("处理").performClick()
        compose.onNodeWithText("使用安全默认值").assertIsDisplayed()
        compose.onNodeWithText("停止并等待").assertIsDisplayed()
        compose.onNodeWithText("自定义回答").assertDoesNotExist()

        compose.runOnUiThread {
            state = state.copy(attention = PanelState(listOf(questionAttention(freeFormAllowed = true))))
        }
        compose.onNodeWithText("自定义回答").performTextInput("保留工作树并继续")
        compose.onNodeWithText("提交回答").performClick()

        assertEquals(
            UiAction.UpdateAttentionDraft(ATTENTION_ID, "保留工作树并继续"),
            actions.filterIsInstance<UiAction.UpdateAttentionDraft>().last(),
        )
        assertEquals(
            UiAction.RespondAttention(ATTENTION_ID, null, "保留工作树并继续"),
            actions.filterIsInstance<UiAction.RespondAttention>().last(),
        )
    }

    @Test
    fun questionSetRendersEveryPromptAndSubmitsKeyedAnswersTogether() {
        val actions = CopyOnWriteArrayList<UiAction>()
        compose.setContent {
            OneKaleidoscopeApp(
                state = verticalSliceState().copy(
                    attention = PanelState(listOf(questionSetAttention())),
                ),
                onAction = actions::add,
            )
        }

        compose.onNode(hasText("待处理") and hasClickAction()).performClick()
        compose.onNodeWithText("处理").performClick()
        compose.onNodeWithText("选择语言").assertIsDisplayed()
        compose.onNodeWithText("要包含哪些").assertIsDisplayed()
        compose.onNodeWithText("Rust").performClick()
        compose.onNodeWithText("测试").performClick()
        compose.onNodeWithText("本题自定义回答").performTextInput("覆盖率说明")
        compose.onNodeWithText("提交全部回答").performClick()

        assertEquals(
            UiAction.RespondQuestion(
                ATTENTION_ID,
                listOf(
                    QuestionAnswerDraftUi("language", listOf("rust"), null),
                    QuestionAnswerDraftUi("details", listOf("tests"), "覆盖率说明"),
                ),
            ),
            actions.filterIsInstance<UiAction.RespondQuestion>().single(),
        )
    }

    @Test
    fun approvalOptionIsForwardedVerbatimRatherThanReinterpretedByUi() {
        val actions = CopyOnWriteArrayList<UiAction>()
        val approval = AttentionUi(
            id = ATTENTION_ID,
            projectName = "OneKaleidoscope",
            sessionTitle = "R3 Android",
            subject = AttentionSubjectUi.Approval(
                summary = "允许写入限定工作树？",
                detail = "仅影响当前任务分支",
                joinWarning = null,
                options = listOf(
                    DecisionOptionUi("runtime-allow-once", "仅本次批准", DecisionToneUi.Positive),
                    DecisionOptionUi("runtime-deny", "拒绝", DecisionToneUi.Destructive),
                ),
            ),
            expiresLabel = "2 分钟",
            responseAvailability = ActionAvailability.Enabled,
        )
        compose.setContent {
            OneKaleidoscopeApp(
                state = verticalSliceState().copy(attention = PanelState(listOf(approval))),
                onAction = actions::add,
            )
        }

        compose.onNode(hasText("待处理") and hasClickAction()).performClick()
        compose.onNodeWithText("处理").performClick()
        compose.onNodeWithText("仅本次批准").performClick()

        assertEquals(
            UiAction.RespondAttention(ATTENTION_ID, "runtime-allow-once", null),
            actions.filterIsInstance<UiAction.RespondAttention>().single(),
        )
    }

    @Test
    fun narrowNavigationAndSelectedLiveTabSurviveStateRestoration() {
        val restoration = StateRestorationTester(compose)
        restoration.setContent {
            OneKaleidoscopeApp(state = verticalSliceState(), onAction = {})
        }
        navigateToSession()
        compose.onNodeWithText("实时").performClick()
        compose.onNodeWithText("实时活动").assertIsDisplayed()
        compose.onNodeWithText("Transcript").assertDoesNotExist()

        restoration.emulateSavedInstanceStateRestore()

        compose.onNodeWithText("实时活动").assertIsDisplayed()
        compose.onNodeWithText("Transcript").assertDoesNotExist()
    }

    @Test
    fun wideLayoutShowsTranscriptAndLiveActivityTogether() {
        compose.setContent {
            CompositionLocalProvider(LocalDensity provides Density(density = 0.45f, fontScale = 1f)) {
                OneKaleidoscopeApp(state = verticalSliceState(), onAction = {})
            }
        }

        navigateToSession()

        compose.onNodeWithText("Transcript").assertIsDisplayed()
        compose.onNodeWithText("实时活动").assertIsDisplayed()
        compose.onNodeWithText("stream chunk from runtime").assertIsDisplayed()
    }

    private fun navigateToSession() {
        compose.onNode(hasText("项目") and hasClickAction()).performClick()
        compose.onNode(hasText("OneKaleidoscope") and hasClickAction()).performClick()
        compose.onNode(hasText("R3 Android") and hasClickAction()).performClick()
    }

    private fun questionAttention(freeFormAllowed: Boolean) = AttentionUi(
        id = ATTENTION_ID,
        projectName = "OneKaleidoscope",
        sessionTitle = "R3 Android",
        subject = AttentionSubjectUi.Question(
            prompt = "如何继续？",
            options = listOf(
                DecisionOptionUi("safe-default", "使用安全默认值", DecisionToneUi.Positive),
                DecisionOptionUi("stop", "停止并等待", DecisionToneUi.Neutral),
            ),
            freeFormAllowed = freeFormAllowed,
        ),
        expiresLabel = null,
        responseAvailability = ActionAvailability.Enabled,
    )

    private fun questionSetAttention() = AttentionUi(
        id = ATTENTION_ID,
        projectName = "OneKaleidoscope",
        sessionTitle = "R3 Android",
        subject = AttentionSubjectUi.Question(
            questions = listOf(
                QuestionPromptUi(
                    key = "language",
                    prompt = "选择语言",
                    options = listOf(
                        DecisionOptionUi("rust", "Rust", DecisionToneUi.Positive),
                        DecisionOptionUi("python", "Python", DecisionToneUi.Neutral),
                    ),
                    multiSelect = false,
                    freeFormAllowed = false,
                ),
                QuestionPromptUi(
                    key = "details",
                    prompt = "要包含哪些",
                    options = listOf(
                        DecisionOptionUi("tests", "测试", DecisionToneUi.Neutral),
                        DecisionOptionUi("docs", "文档", DecisionToneUi.Neutral),
                    ),
                    multiSelect = true,
                    freeFormAllowed = true,
                ),
            ),
        ),
        expiresLabel = null,
        responseAvailability = ActionAvailability.Enabled,
    )

    private fun verticalSliceState(): AppUiState {
        val project = ProjectUi(
            id = PROJECT_ID,
            displayName = "OneKaleidoscope",
            providerLabel = "Codex",
            sessionTotal = 1,
            runningCount = 1,
            waitingHumanCount = 0,
            attentionCount = 1,
            lastActivityLabel = "刚刚",
        )
        val session = SessionUi(
            id = SESSION_ID,
            title = "R3 Android",
            status = SessionStatusUi.Running,
            ownershipLabel = "Broker 管理",
            liveBindingLabel = "实时控制",
            isLive = true,
            queueDepth = 1,
            openAttentionCount = 1,
            lastActivityLabel = "刚刚",
            runtimeId = "runtime-1",
        )
        val transcriptItem = TranscriptItemUi(
            id = "item-transcript",
            kind = TranscriptItemKind.Agent,
            statusLabel = "完成",
            text = "persisted transcript",
        )
        val streamingItem = TranscriptItemUi(
            id = "item-stream",
            kind = TranscriptItemKind.Agent,
            statusLabel = "流式",
            text = "stream chunk from runtime",
        )
        return AppUiState(
            connection = ConnectionUiState.Live("Workstation", "TLS 1.3 · LAN"),
            projects = PanelState(listOf(project)),
            selectedProjectId = PROJECT_ID,
            sessions = PanelState(SessionSectionsUi(active = listOf(session))),
            selectedSessionId = SESSION_ID,
            transcript = PanelState(
                TranscriptUi(
                    sessionId = SESSION_ID,
                    turns = listOf(
                        TranscriptTurnUi(
                            id = "turn-1",
                            statusLabel = "完成",
                            originLabel = "手机输入",
                            items = listOf(transcriptItem),
                        ),
                    ),
                    hasEarlier = false,
                ),
            ),
            liveActivity = PanelState(
                LiveActivityUi(
                    sessionId = SESSION_ID,
                    activeTurnId = "turn-live",
                    streamingItems = listOf(streamingItem),
                    plan = emptyList(),
                    tasks = emptyList(),
                    updatedLabel = "刚刚",
                ),
            ),
            queue = PanelState(
                InputQueueUi(
                    sessionId = SESSION_ID,
                    entries = listOf(
                        QueueEntryUi(
                            id = "queue-1",
                            position = 0,
                            intent = QueueIntentUi.NewTurn,
                            bodyText = "run the Android checks",
                            state = QueueStateUi.Pending,
                            editable = false,
                        ),
                    ),
                    writable = true,
                    writableReason = null,
                    steerSupported = false,
                ),
            ),
            attention = PanelState(listOf(questionAttention(freeFormAllowed = true))),
            selectedRuntimeId = "runtime-1",
            promptAction = ActionAvailability.Enabled,
            enqueueNewTurnAction = ActionAvailability.Enabled,
            enqueueSteerAction = ActionAvailability.disabled("Runtime 未证明 TurnSteer"),
        )
    }

    companion object {
        private const val PROJECT_ID = "project-1"
        private const val SESSION_ID = "session-1"
        private const val ATTENTION_ID = "attention-1"
    }
}
