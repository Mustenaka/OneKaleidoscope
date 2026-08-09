package com.onekaleidoscope.ui

import android.content.res.Configuration
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.tooling.preview.Preview

@Preview(name = "Narrow light", widthDp = 360, heightDp = 800, showBackground = true)
@Preview(
    name = "Narrow dark large text",
    widthDp = 360,
    heightDp = 800,
    fontScale = 1.5f,
    uiMode = Configuration.UI_MODE_NIGHT_YES,
    showBackground = true,
)
@Composable
private fun HostsPreview() {
    OneKaleidoscopeApp(state = previewState(), onAction = {})
}

@Preview(name = "Landscape session", widthDp = 840, heightDp = 480, showBackground = true)
@Composable
private fun SessionPreview() {
    KaleidoscopeTheme {
        SessionScreen(previewState(), onAction = {}, modifier = Modifier.fillMaxSize())
    }
}

@Preview(name = "Attention inbox", widthDp = 360, heightDp = 800, showBackground = true)
@Composable
private fun AttentionPreview() {
    KaleidoscopeTheme {
        AttentionScreen(
            panel = previewState().attention,
            drafts = emptyMap(),
            onAction = {},
            modifier = Modifier.fillMaxSize(),
        )
    }
}

@Preview(name = "Capability states", widthDp = 360, heightDp = 800, showBackground = true)
@Composable
private fun CapabilityPreview() {
    KaleidoscopeTheme {
        CapabilitiesScreen(previewState().capabilities, onAction = {}, modifier = Modifier.fillMaxSize())
    }
}

private fun previewState(): AppUiState = AppUiState(
    connection = ConnectionUiState.Live("Workstation", "192.168.1.8 · TLS"),
    hosts = listOf(HostUi("host-1", "Workstation", "Windows", ReachabilityUi.LanDirect, "刚刚")),
    selectedHostId = "host-1",
    projects = PanelState(
        value = listOf(ProjectUi("project-1", "OneKaleidoscope", "Codex", 4, 1, 1, 1, "刚刚")),
    ),
    selectedProjectId = "project-1",
    sessions = PanelState(
        value = SessionSectionsUi(
            active = listOf(
                SessionUi(
                    "session-1",
                    "Android 局域网纵切",
                    SessionStatusUi.WaitingApproval,
                    "Broker 管理",
                    "实时观察中",
                    true,
                    2,
                    1,
                    "刚刚",
                    "runtime-1",
                ),
            ),
        ),
    ),
    selectedSessionId = "session-1",
    transcript = PanelState(
        value = TranscriptUi(
            sessionId = "session-1",
            turns = listOf(
                TranscriptTurnUi(
                    "turn-1",
                    "运行中",
                    "远程命令",
                    listOf(
                        TranscriptItemUi("item-1", TranscriptItemKind.User, "已完成", "请实现 Android 界面。"),
                        TranscriptItemUi("item-2", TranscriptItemKind.Agent, "进行中", "正在连接投影和界面状态……"),
                    ),
                ),
            ),
            hasEarlier = true,
        ),
    ),
    liveActivity = PanelState(
        value = LiveActivityUi(
            "session-1",
            "turn-1",
            listOf(TranscriptItemUi("item-2", TranscriptItemKind.Agent, "进行中", "正在连接投影和界面状态……")),
            listOf(ProgressEntryUi("实现 Compose 页面", "进行中")),
            listOf(ProgressEntryUi("检查无障碍", "等待中")),
            "刚刚",
        ),
    ),
    queue = PanelState(
        value = InputQueueUi(
            "session-1",
            listOf(
                QueueEntryUi("queue-1", 0, QueueIntentUi.NewTurn, "完成后运行测试", state = QueueStateUi.Pending, editable = true),
                QueueEntryUi("queue-2", 1, QueueIntentUi.SteerActiveTurn, "先修编译错误", state = QueueStateUi.Submitting, editable = false),
            ),
            writable = true,
            writableReason = null,
            steerSupported = false,
        ),
    ),
    attention = PanelState(
        value = listOf(
            AttentionUi(
                "attention-1",
                "OneKaleidoscope",
                "Android 局域网纵切",
                AttentionSubjectUi.Approval(
                    "允许修改 Compose 界面文件？",
                    "范围仅限 apps/android/app/src/main/java/com/onekaleidoscope/ui。",
                    null,
                    listOf(
                        DecisionOptionUi("allow", "仅本次允许", DecisionToneUi.Positive),
                        DecisionOptionUi("deny", "拒绝", DecisionToneUi.Destructive),
                    ),
                ),
                "5 分钟后",
                ActionAvailability.Enabled,
            ),
        ),
    ),
    capabilities = PanelState(
        value = RuntimeCapabilitiesUi(
            "runtime-1",
            "Codex runtime",
            "刚刚",
            listOf(
                CapabilityUi("prompt", "发送新回合", CapabilityStateUi.Supported, "运行时已确认", "观察到的流量"),
                CapabilityUi("steer", "引导当前回合", CapabilityStateUi.NotVerified, "尚无有效证据", "无"),
                CapabilityUi("attach", "原生 GUI 附着", CapabilityStateUi.UpstreamBlocked, "公开接口不存在", "阻塞项 D-B"),
            ),
        ),
    ),
    selectedRuntimeId = "runtime-1",
    draft = "补充一条输入",
    promptAction = ActionAvailability.Enabled,
    enqueueNewTurnAction = ActionAvailability.Enabled,
    enqueueSteerAction = ActionAvailability.disabled("当前 live binding 未证明 TurnSteer"),
)
