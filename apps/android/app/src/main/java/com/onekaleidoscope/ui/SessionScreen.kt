package com.onekaleidoscope.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Card
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Surface
import androidx.compose.material3.Tab
import androidx.compose.material3.TabRow
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp

@Composable
internal fun SessionScreen(state: AppUiState, onAction: (UiAction) -> Unit, modifier: Modifier = Modifier) {
    var selectedTab by rememberSaveable(state.selectedSessionId) { mutableIntStateOf(0) }
    val tabs = listOf("对话", "实时", "队列")
    Column(modifier.fillMaxSize()) {
        TabRow(selectedTabIndex = selectedTab) {
            tabs.forEachIndexed { index, title ->
                Tab(
                    selected = selectedTab == index,
                    onClick = { selectedTab = index },
                    modifier = Modifier.heightIn(min = 48.dp),
                    text = { Text(title) },
                )
            }
        }
        BoxWithConstraints(Modifier.fillMaxSize()) {
            val wide = maxWidth >= 720.dp
            if (wide && selectedTab != 2) {
                Row(Modifier.fillMaxSize()) {
                    TranscriptPanel(state.transcript, onAction, Modifier.weight(1.2f))
                    LivePanel(state.liveActivity, onAction, Modifier.weight(0.8f))
                }
            } else {
                when (selectedTab) {
                    0 -> TranscriptPanel(state.transcript, onAction, Modifier.fillMaxSize())
                    1 -> LivePanel(state.liveActivity, onAction, Modifier.fillMaxSize())
                    else -> QueuePanel(state.queue, onAction, Modifier.fillMaxSize())
                }
            }
        }
    }
}

@Composable
private fun TranscriptPanel(
    panel: PanelState<TranscriptUi>,
    onAction: (UiAction) -> Unit,
    modifier: Modifier,
) {
    val transcript = panel.value
    PanelList(
        title = "Transcript",
        freshness = panel.freshness,
        loading = panel.loading,
        error = panel.error,
        onRefresh = { onAction(UiAction.Refresh) },
        modifier = modifier,
        empty = transcript?.turns.isNullOrEmpty(),
        emptyText = "还没有对话内容。",
    ) {
        if (transcript?.hasEarlier == true) {
            item { Text("更早内容尚未载入", style = MaterialTheme.typography.labelMedium) }
        }
        transcript?.turns.orEmpty().forEach { turn ->
            item(turn.id) {
                Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                    Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                        Text("回合", style = MaterialTheme.typography.titleSmall)
                        StatusPill(turn.statusLabel, StatusTone.Neutral)
                    }
                    Text(turn.originLabel, style = MaterialTheme.typography.labelSmall)
                    turn.errorSummary?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                }
            }
            items(turn.items, key = { it.id }) { item -> TranscriptItemCard(item) }
            item("divider-${turn.id}") { HorizontalDivider() }
        }
    }
}

@Composable
private fun TranscriptItemCard(item: TranscriptItemUi) {
    val label = when (item.kind) {
        TranscriptItemKind.Unknown -> "活动项"
        TranscriptItemKind.User -> "你"
        TranscriptItemKind.Agent -> "Agent"
        TranscriptItemKind.Reasoning -> "推理"
        TranscriptItemKind.Tool -> "工具"
        TranscriptItemKind.FileEdit -> "文件变更"
        TranscriptItemKind.Plan -> "计划"
        TranscriptItemKind.Task -> "任务"
        TranscriptItemKind.Diagnostic -> "诊断"
    }
    val container = when (item.kind) {
        TranscriptItemKind.User -> MaterialTheme.colorScheme.primaryContainer
        TranscriptItemKind.Diagnostic -> MaterialTheme.colorScheme.errorContainer
        else -> MaterialTheme.colorScheme.surfaceContainer
    }
    Card(Modifier.fillMaxWidth()) {
        Column(
            Modifier.fillMaxWidth().padding(14.dp),
            verticalArrangement = Arrangement.spacedBy(6.dp),
        ) {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text(label, fontWeight = FontWeight.SemiBold)
                Text(item.statusLabel, style = MaterialTheme.typography.labelSmall)
            }
            Surface(color = container, shape = MaterialTheme.shapes.small) {
                Text(
                    item.text ?: item.contentUnavailableReason ?: "正文不可用",
                    Modifier.fillMaxWidth().padding(10.dp),
                    style = MaterialTheme.typography.bodyMedium,
                    fontStyle = if (item.text == null) FontStyle.Italic else FontStyle.Normal,
                )
            }
        }
    }
}

@Composable
private fun LivePanel(
    panel: PanelState<LiveActivityUi>,
    onAction: (UiAction) -> Unit,
    modifier: Modifier,
) {
    val live = panel.value
    PanelList(
        title = "实时活动",
        freshness = panel.freshness,
        loading = panel.loading,
        error = panel.error,
        onRefresh = { onAction(UiAction.Refresh) },
        modifier = modifier,
        empty = live == null,
        emptyText = "当前没有实时活动。历史内容不会冒充实时附着。",
    ) {
        item {
            StatusPill(
                label = if (live?.activeTurnId == null) "无活动回合" else "活动回合",
                tone = if (live?.activeTurnId == null) StatusTone.Neutral else StatusTone.Positive,
            )
        }
        if (live != null) {
            if (live.streamingItems.isNotEmpty()) {
                item { SectionHeading("流式输出") }
                items(live.streamingItems, key = { it.id }) { TranscriptItemCard(it) }
            }
            if (live.plan.isNotEmpty()) {
                item { SectionHeading("计划") }
                items(live.plan, key = { "plan-${it.title}" }) { ProgressRow(it) }
            }
            if (live.tasks.isNotEmpty()) {
                item { SectionHeading("Agent 任务") }
                items(live.tasks, key = { "task-${it.title}" }) { ProgressRow(it) }
            }
            item { Text("更新于 ${live.updatedLabel}", style = MaterialTheme.typography.labelSmall) }
        }
    }
}

@Composable
private fun ProgressRow(entry: ProgressEntryUi) {
    Card(Modifier.fillMaxWidth()) {
        Row(
            Modifier.fillMaxWidth().padding(14.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Text(entry.title, Modifier.weight(1f))
            StatusPill(entry.stateLabel, StatusTone.Neutral)
        }
    }
}

@Composable
private fun QueuePanel(
    panel: PanelState<InputQueueUi>,
    onAction: (UiAction) -> Unit,
    modifier: Modifier,
) {
    val queue = panel.value
    PanelList(
        title = "输入队列",
        freshness = panel.freshness,
        loading = panel.loading,
        error = panel.error,
        onRefresh = { onAction(UiAction.Refresh) },
        modifier = modifier,
        empty = queue == null || (queue.entries.isEmpty() && queue.writable),
        emptyText = "队列为空。Pending 与 Submitting 都不会显示成已送达。",
    ) {
        if (queue != null && !queue.writable) {
            item {
                Surface(
                    color = MaterialTheme.colorScheme.tertiaryContainer,
                    shape = MaterialTheme.shapes.medium,
                ) {
                    Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(4.dp)) {
                        Text("队列只读", fontWeight = FontWeight.SemiBold)
                        Text(queue.writableReason ?: "当前连接不允许写入队列")
                    }
                }
            }
        }
        items(queue?.entries.orEmpty(), key = { it.id }) { entry ->
            QueueEntryCard(entry)
        }
    }
}

@Composable
private fun QueueEntryCard(entry: QueueEntryUi) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text("#${entry.position + 1} · ${entry.intent.label()}", fontWeight = FontWeight.SemiBold)
                StatusPill(entry.state.label(), entry.state.tone())
            }
            Text(
                entry.bodyText ?: entry.bodyUnavailableReason ?: "正文不可用",
                style = MaterialTheme.typography.bodyMedium,
                fontStyle = if (entry.bodyText == null) FontStyle.Italic else FontStyle.Normal,
            )
            if (!entry.editable && entry.state == QueueStateUi.Pending) {
                Text("此条目当前不可编辑", style = MaterialTheme.typography.labelSmall)
            }
        }
    }
}

@Composable
internal fun PromptComposer(state: AppUiState, onAction: (UiAction) -> Unit, modifier: Modifier = Modifier) {
    Surface(modifier.fillMaxWidth(), tonalElevation = 3.dp, shadowElevation = 3.dp) {
        Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                AvailabilityButton(
                    label = "恢复历史会话",
                    availability = state.resumeAction,
                    onClick = { onAction(UiAction.ResumeSession) },
                    modifier = Modifier.weight(1f),
                    prominent = false,
                )
                AvailabilityButton(
                    label = "中断当前回合",
                    availability = state.interruptAction,
                    onClick = { onAction(UiAction.InterruptTurn) },
                    modifier = Modifier.weight(1f),
                    prominent = false,
                )
            }
            OutlinedTextField(
                value = state.draft,
                onValueChange = { onAction(UiAction.UpdateDraft(it)) },
                modifier = Modifier.fillMaxWidth(),
                label = { Text("给 Agent 的输入") },
                minLines = 2,
                maxLines = 6,
            )
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                AvailabilityButton(
                    label = "发送新回合",
                    availability = state.promptAction,
                    onClick = { onAction(UiAction.SubmitPrompt) },
                    modifier = Modifier.weight(1f),
                )
                AvailabilityButton(
                    label = "加入队列",
                    availability = state.enqueueNewTurnAction,
                    onClick = { onAction(UiAction.EnqueueInput(QueueIntentUi.NewTurn)) },
                    modifier = Modifier.weight(1f),
                    prominent = false,
                )
            }
            AvailabilityButton(
                label = "尝试引导当前回合",
                availability = state.enqueueSteerAction,
                onClick = { onAction(UiAction.EnqueueInput(QueueIntentUi.SteerActiveTurn)) },
                prominent = false,
            )
        }
    }
}

@Composable
private fun SectionHeading(title: String) {
    Text(title, Modifier.semantics { heading() }, style = MaterialTheme.typography.titleMedium)
}

private fun QueueIntentUi.label(): String = when (this) {
    QueueIntentUi.NewTurn -> "新回合"
    QueueIntentUi.SteerActiveTurn -> "引导意图"
}

private fun QueueStateUi.label(): String = when (this) {
    QueueStateUi.Pending -> "等待中"
    QueueStateUi.Submitting -> "提交中"
    QueueStateUi.DeliveredNewTurn -> "已作为新回合送达"
    QueueStateUi.DeliveredSteer -> "已确认引导"
    QueueStateUi.Rejected -> "已拒绝"
    QueueStateUi.Cancelled -> "已取消"
}

private fun QueueStateUi.tone(): StatusTone = when (this) {
    QueueStateUi.DeliveredNewTurn, QueueStateUi.DeliveredSteer -> StatusTone.Positive
    QueueStateUi.Pending, QueueStateUi.Submitting -> StatusTone.Warning
    QueueStateUi.Rejected -> StatusTone.Error
    QueueStateUi.Cancelled -> StatusTone.Neutral
}
