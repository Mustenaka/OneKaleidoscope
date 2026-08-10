package com.onekaleidoscope.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.autofill.ContentType
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalClipboard
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.semantics.contentType
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.input.KeyboardCapitalization
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.launch

@Composable
internal fun HostsScreen(
    connection: ConnectionUiState,
    hosts: List<HostUi>,
    onAction: (UiAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    var pairingPayload by remember { mutableStateOf("") }
    val clipboard = LocalClipboard.current
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    PanelList(
        title = "电脑与配对",
        freshness = DataFreshness.Live,
        loading = connection is ConnectionUiState.Pairing,
        error = (connection as? ConnectionUiState.Error)?.safeSummary,
        onRefresh = { onAction(UiAction.RetryConnection) },
        modifier = modifier,
        empty = false,
    ) {
        item { ConnectionCard(connection, onAction) }
        item {
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                Text("添加电脑", style = MaterialTheme.typography.titleMedium)
                Text(
                    "扫描 hostd 显示的二维码，或粘贴完整配对内容。配对内容不会保存到草稿。",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                OutlinedTextField(
                    value = pairingPayload,
                    onValueChange = { pairingPayload = it },
                    label = { Text("配对二维码内容") },
                    placeholder = { Text("kaleido://pair/…") },
                    singleLine = true,
                    keyboardOptions = KeyboardOptions(
                        capitalization = KeyboardCapitalization.None,
                        keyboardType = KeyboardType.Uri,
                    ),
                    modifier = Modifier.fillMaxWidth().semantics { contentType = ContentType.NewPassword },
                )
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    OutlinedButton(
                        onClick = {
                            scope.launch {
                                pairingPayload = clipboard.getClipEntry()
                                    ?.clipData
                                    ?.getItemAt(0)
                                    ?.coerceToText(context)
                                    ?.toString()
                                    .orEmpty()
                            }
                        },
                        modifier = Modifier.weight(1f).heightIn(min = 48.dp),
                    ) { Text("从剪贴板粘贴") }
                    androidx.compose.material3.Button(
                        onClick = {
                            val payload = pairingPayload
                            pairingPayload = ""
                            onAction(UiAction.SubmitPairingQr(payload))
                        },
                        enabled = pairingPayload.isNotBlank(),
                        modifier = Modifier.weight(1f).heightIn(min = 48.dp),
                    ) { Text("开始配对") }
                }
            }
        }
        if (hosts.isEmpty()) {
            item { EmptyPanel("尚未配对电脑。电脑端启动 hostd 后即可添加。") }
        } else {
            item { Text("已配对电脑", style = MaterialTheme.typography.titleMedium) }
            items(hosts.size, key = { hosts[it].id }) { index ->
                val host = hosts[index]
                HostCard(host = host, onAction = onAction)
            }
        }
    }
}

@Composable
private fun ConnectionCard(connection: ConnectionUiState, onAction: (UiAction) -> Unit) {
    val (title, detail, tone) = when (connection) {
        ConnectionUiState.Initializing -> Triple("正在初始化", "正在打开安全凭据与离线缓存。", StatusTone.Neutral)
        ConnectionUiState.Unpaired -> Triple("未连接", "先添加一台电脑。", StatusTone.Neutral)
        is ConnectionUiState.Pairing -> Triple("正在配对", connection.stage, StatusTone.Warning)
        is ConnectionUiState.Connecting -> Triple(
            "正在连接 ${connection.hostName}",
            "正在建立端到端认证连接；尚未发布在线路径。",
            StatusTone.Warning,
        )
        is ConnectionUiState.Live -> Triple("已连接 ${connection.hostName}", connection.endpointLabel, StatusTone.Positive)
        is ConnectionUiState.Offline -> Triple("离线", connection.reason, StatusTone.Warning)
        is ConnectionUiState.Revoked -> Triple("设备已被吊销", connection.reason, StatusTone.Error)
        is ConnectionUiState.Error -> Triple("连接失败", connection.safeSummary, StatusTone.Error)
    }
    Card(colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceContainer)) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            StatusPill(title, tone)
            Text(detail, style = MaterialTheme.typography.bodyMedium)
            if (connection is ConnectionUiState.Offline && connection.cachedDataAvailable) {
                Text("可继续查看明确标记的离线缓存。", style = MaterialTheme.typography.bodySmall)
            }
            if (connection is ConnectionUiState.Error && connection.retryable || connection is ConnectionUiState.Offline) {
                OutlinedButton(
                    onClick = { onAction(UiAction.RetryConnection) },
                    modifier = Modifier.heightIn(min = 48.dp),
                ) { Text("重新连接") }
            }
        }
    }
}

@Composable
private fun HostCard(host: HostUi, onAction: (UiAction) -> Unit) {
    val live = host.reachability != ReachabilityUi.Offline
    Card(
        onClick = {
            onAction(UiAction.SelectHost(host.id))
            if (live) onAction(UiAction.Navigate(AppDestination.Projects))
        },
        modifier = Modifier.fillMaxWidth().heightIn(min = 72.dp),
    ) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Column(Modifier.weight(1f)) {
                    Text(host.displayName, style = MaterialTheme.typography.titleMedium)
                    Text("${host.platform} · ${host.lastSeenLabel}", style = MaterialTheme.typography.bodySmall)
                }
                StatusDot(
                    label = if (live) reachabilityLabel(host.reachability) else "离线",
                    color = if (live) Color(0xFF2E7D32) else MaterialTheme.colorScheme.outline,
                )
            }
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                TextButton(
                    onClick = {
                        if (live) onAction(UiAction.DisconnectHost(host.id)) else onAction(UiAction.ConnectHost(host.id))
                    },
                    modifier = Modifier.heightIn(min = 48.dp),
                ) { Text(if (live) "断开" else "连接") }
                TextButton(
                    onClick = { onAction(UiAction.ForgetHost(host.id)) },
                    modifier = Modifier.heightIn(min = 48.dp),
                ) { Text("移除") }
            }
        }
    }
}

@Composable
internal fun ProjectsScreen(
    panel: PanelState<List<ProjectUi>>,
    onAction: (UiAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val projects = panel.value.orEmpty()
    val groups = projects.groupBy { it.providerLabel }
    PanelList(
        title = "项目",
        freshness = panel.freshness,
        loading = panel.loading,
        error = panel.error,
        onRefresh = { onAction(UiAction.Refresh) },
        modifier = modifier,
        empty = projects.isEmpty(),
        emptyText = "这台电脑还没有可显示的项目。",
    ) {
        groups.forEach { (provider, providerProjects) ->
            item(provider) { Text(provider, style = MaterialTheme.typography.titleMedium) }
            items(providerProjects.size, key = { providerProjects[it].id }) { index ->
                val project = providerProjects[index]
                Card(
                    onClick = {
                        onAction(UiAction.SelectProject(project.id))
                        onAction(UiAction.Navigate(AppDestination.Sessions))
                    },
                    modifier = Modifier.fillMaxWidth().heightIn(min = 88.dp),
                ) {
                    Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                        Text(project.displayName, style = MaterialTheme.typography.titleMedium)
                        Text(
                            "${project.sessionTotal} 个会话 · ${project.runningCount} 运行中 · ${project.waitingHumanCount} 等待人工",
                            style = MaterialTheme.typography.bodySmall,
                        )
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            if (project.attentionCount > 0) StatusPill("${project.attentionCount} 项待处理", StatusTone.Warning)
                            Text(project.lastActivityLabel, style = MaterialTheme.typography.labelMedium)
                        }
                    }
                }
            }
        }
    }
}

@Composable
internal fun SessionsScreen(
    panel: PanelState<SessionSectionsUi>,
    onAction: (UiAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val sections = panel.value ?: SessionSectionsUi()
    val empty = sections.active.isEmpty() && sections.history.isEmpty() && sections.archived.isEmpty()
    PanelList(
        title = "会话",
        freshness = panel.freshness,
        loading = panel.loading,
        error = panel.error,
        onRefresh = { onAction(UiAction.Refresh) },
        modifier = modifier,
        empty = empty,
        emptyText = "这个项目还没有会话。",
    ) {
        sessionSection("活动", sections.active, onAction)
        sessionSection("历史", sections.history, onAction)
        sessionSection("已归档", sections.archived, onAction)
    }
}

private fun androidx.compose.foundation.lazy.LazyListScope.sessionSection(
    title: String,
    sessions: List<SessionUi>,
    onAction: (UiAction) -> Unit,
) {
    if (sessions.isEmpty()) return
    item(title) { Text(title, style = MaterialTheme.typography.titleMedium) }
    items(sessions.size, key = { sessions[it].id }) { index ->
        val session = sessions[index]
        Card(
            onClick = {
                onAction(UiAction.SelectSession(session.id))
                onAction(UiAction.Navigate(AppDestination.Session))
            },
            modifier = Modifier.fillMaxWidth().heightIn(min = 88.dp),
        ) {
            Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                    Text(session.title, Modifier.weight(1f), style = MaterialTheme.typography.titleMedium)
                    StatusPill(session.status.label(), session.status.tone())
                }
                Text(
                    "${session.ownershipLabel} · ${session.liveBindingLabel}",
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (session.queueDepth > 0) StatusPill("队列 ${session.queueDepth}", StatusTone.Neutral)
                    if (session.openAttentionCount > 0) StatusPill("待处理 ${session.openAttentionCount}", StatusTone.Warning)
                    Text(session.lastActivityLabel, style = MaterialTheme.typography.labelMedium)
                }
            }
        }
    }
}

private fun SessionStatusUi.label(): String = when (this) {
    SessionStatusUi.Offline -> "离线"
    SessionStatusUi.Idle -> "空闲"
    SessionStatusUi.Running -> "运行中"
    SessionStatusUi.WaitingUser -> "等待回答"
    SessionStatusUi.WaitingApproval -> "等待审批"
    SessionStatusUi.Queued -> "已排队"
    SessionStatusUi.Failed -> "失败"
    SessionStatusUi.Completed -> "已完成"
    SessionStatusUi.Cancelled -> "已取消"
}

private fun SessionStatusUi.tone(): StatusTone = when (this) {
    SessionStatusUi.Running -> StatusTone.Positive
    SessionStatusUi.WaitingUser, SessionStatusUi.WaitingApproval, SessionStatusUi.Queued -> StatusTone.Warning
    SessionStatusUi.Failed -> StatusTone.Error
    else -> StatusTone.Neutral
}

private fun reachabilityLabel(reachability: ReachabilityUi): String = when (reachability) {
    ReachabilityUi.Offline -> "离线"
    ReachabilityUi.LanDirect -> "局域网直连"
    ReachabilityUi.PeerToPeer -> "点对点"
    ReachabilityUi.Relayed -> "自有中继"
}
