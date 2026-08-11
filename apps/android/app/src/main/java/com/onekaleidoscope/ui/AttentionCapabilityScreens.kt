package com.onekaleidoscope.ui

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp

internal fun questionPromptTestTag(attentionId: String, questionKey: String): String =
    "attention-question-prompt:$attentionId:$questionKey"

internal fun questionOptionTestTag(attentionId: String, questionKey: String, optionId: String): String =
    "attention-question-option:$attentionId:$questionKey:$optionId"

internal fun questionFreeFormTestTag(attentionId: String, questionKey: String): String =
    "attention-question-free-form:$attentionId:$questionKey"

internal fun questionSubmitTestTag(attentionId: String): String =
    "attention-question-submit:$attentionId"

@Composable
internal fun AttentionScreen(
    panel: PanelState<List<AttentionUi>>,
    drafts: Map<String, String>,
    questionDrafts: Map<String, List<QuestionAnswerDraftUi>>,
    onAction: (UiAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    var respondingTo by rememberSaveable { mutableStateOf<String?>(null) }
    val entries = panel.value.orEmpty()
    val selected = entries.firstOrNull { it.id == respondingTo }
    PanelList(
        title = "待处理",
        freshness = panel.freshness,
        loading = panel.loading,
        error = panel.error,
        onRefresh = { onAction(UiAction.Refresh) },
        modifier = modifier,
        empty = entries.isEmpty(),
        emptyText = "目前没有等待人工处理的事项。",
    ) {
        items(entries.size, key = { entries[it].id }) { index ->
            val attention = entries[index]
            AttentionCard(attention = attention, onRespond = { respondingTo = attention.id })
        }
    }
    if (selected != null) {
        ResponseDialog(
            attention = selected,
            draft = drafts[selected.id].orEmpty(),
            questionDrafts = questionDrafts[selected.id].orEmpty(),
            onDraftChange = { onAction(UiAction.UpdateAttentionDraft(selected.id, it)) },
            onDismiss = { respondingTo = null },
            onRespond = { optionId, freeForm ->
                onAction(UiAction.RespondAttention(selected.id, optionId, freeForm))
            },
            onRespondQuestion = { answers ->
                onAction(UiAction.RespondQuestion(selected.id, answers))
            },
            onQuestionDraftChange = { answer ->
                onAction(UiAction.UpdateQuestionDraft(selected.id, answer))
            },
        )
    }
}

@Composable
private fun AttentionCard(attention: AttentionUi, onRespond: () -> Unit) {
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(16.dp), verticalArrangement = Arrangement.spacedBy(8.dp)) {
            Text(attention.subject.title(), style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.SemiBold)
            Text(attention.projectName, style = MaterialTheme.typography.labelMedium)
            attention.sessionTitle?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
            when (val subject = attention.subject) {
                is AttentionSubjectUi.Approval -> {
                    Text(subject.summary ?: "审批摘要不可用")
                    subject.joinWarning?.let {
                        StatusPill(it, StatusTone.Warning)
                    }
                }
                is AttentionSubjectUi.Question -> {
                    if (subject.questions.isEmpty()) {
                        Text(subject.prompt ?: "问题正文不可用")
                    } else {
                        subject.questions.forEach { question ->
                            Text(question.prompt ?: "问题正文不可用")
                        }
                    }
                }
                is AttentionSubjectUi.ConnectionFault -> {
                    Text(subject.runtimeLabel)
                    Text(subject.safeReason, color = MaterialTheme.colorScheme.error)
                }
            }
            attention.expiresLabel?.let { Text("到期：$it", style = MaterialTheme.typography.labelSmall) }
            if (attention.subject !is AttentionSubjectUi.ConnectionFault) {
                AvailabilityButton(
                    label = "处理",
                    availability = attention.responseAvailability,
                    onClick = onRespond,
                )
            }
        }
    }
}

@Composable
private fun ResponseDialog(
    attention: AttentionUi,
    draft: String,
    questionDrafts: List<QuestionAnswerDraftUi>,
    onDraftChange: (String) -> Unit,
    onDismiss: () -> Unit,
    onRespond: (String?, String?) -> Unit,
    onRespondQuestion: (List<QuestionAnswerDraftUi>) -> Unit,
    onQuestionDraftChange: (QuestionAnswerDraftUi) -> Unit,
) {
    val subject = attention.subject
    val options = when (subject) {
        is AttentionSubjectUi.Approval -> subject.options
        is AttentionSubjectUi.Question -> subject.options
        is AttentionSubjectUi.ConnectionFault -> emptyList()
    }
    val freeFormAllowed = subject is AttentionSubjectUi.Question && subject.freeFormAllowed
    val questionPrompts = (subject as? AttentionSubjectUi.Question)?.questions.orEmpty()
    val draftsByQuestion = questionDrafts.associateBy(QuestionAnswerDraftUi::questionKey)
    val allQuestionsReady = questionPrompts.isNotEmpty() && questionPrompts.all { question ->
        val selected = draftsByQuestion[question.key]?.optionIds.orEmpty()
        val freeForm = draftsByQuestion[question.key]?.freeForm.orEmpty().trim()
        selected.isNotEmpty() || (question.freeFormAllowed && freeForm.isNotEmpty())
    }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(subject.title()) },
        text = {
            Column(
                modifier = Modifier.verticalScroll(rememberScrollState()),
                verticalArrangement = Arrangement.spacedBy(10.dp),
            ) {
                when (subject) {
                    is AttentionSubjectUi.Approval -> {
                        Text(subject.summary ?: "审批摘要不可用")
                        subject.detail?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
                        subject.joinWarning?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                    }
                    is AttentionSubjectUi.Question -> {
                        if (questionPrompts.isEmpty()) {
                            Text(subject.prompt ?: "问题正文不可用")
                        } else {
                            questionPrompts.forEach { question ->
                                QuestionPromptForm(
                                    attentionId = attention.id,
                                    question = question,
                                    selectedOptionIds = draftsByQuestion[question.key]?.optionIds.orEmpty(),
                                    freeForm = draftsByQuestion[question.key]?.freeForm.orEmpty(),
                                    enabled = attention.responseAvailability.enabled,
                                    onOptionToggle = { optionId ->
                                        val current = draftsByQuestion[question.key]?.optionIds.orEmpty()
                                        val next = if (optionId in current) {
                                            current - optionId
                                        } else if (question.multiSelect) {
                                            current + optionId
                                        } else {
                                            listOf(optionId)
                                        }
                                        onQuestionDraftChange(
                                            QuestionAnswerDraftUi(
                                                questionKey = question.key,
                                                optionIds = next,
                                                freeForm = draftsByQuestion[question.key]?.freeForm,
                                            ),
                                        )
                                    },
                                    onFreeFormChange = { value ->
                                        onQuestionDraftChange(
                                            QuestionAnswerDraftUi(
                                                questionKey = question.key,
                                                optionIds = draftsByQuestion[question.key]?.optionIds.orEmpty(),
                                                freeForm = value,
                                            ),
                                        )
                                    },
                                )
                            }
                            Button(
                                onClick = {
                                    onRespondQuestion(
                                        questionPrompts.map { question ->
                                            QuestionAnswerDraftUi(
                                                questionKey = question.key,
                                                optionIds = draftsByQuestion[question.key]?.optionIds.orEmpty(),
                                                freeForm = draftsByQuestion[question.key]?.freeForm.orEmpty()
                                                    .trim()
                                                    .takeIf(String::isNotEmpty),
                                            )
                                        },
                                    )
                                },
                                enabled = attention.responseAvailability.enabled && allQuestionsReady,
                                modifier = Modifier
                                    .fillMaxWidth()
                                    .heightIn(min = 48.dp)
                                    .testTag(questionSubmitTestTag(attention.id)),
                            ) { Text("提交全部回答") }
                        }
                    }
                    is AttentionSubjectUi.ConnectionFault -> Text(subject.safeReason)
                }
                if (questionPrompts.isEmpty()) {
                    options.forEach { option ->
                        DecisionButton(
                            option = option,
                            enabled = attention.responseAvailability.enabled,
                            onClick = { onRespond(option.id, null) },
                        )
                    }
                    if (freeFormAllowed) {
                        OutlinedTextField(
                            value = draft,
                            onValueChange = onDraftChange,
                            modifier = Modifier.fillMaxWidth(),
                            label = { Text("自定义回答") },
                            minLines = 2,
                            maxLines = 6,
                        )
                        Button(
                            onClick = { onRespond(null, draft.trim()) },
                            enabled = attention.responseAvailability.enabled && draft.isNotBlank(),
                            modifier = Modifier.fillMaxWidth().heightIn(min = 48.dp),
                        ) { Text("提交回答") }
                    }
                }
                if (!attention.responseAvailability.enabled) {
                    Text(
                        attention.responseAvailability.disabledReason.orEmpty(),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
        },
        confirmButton = {},
        dismissButton = {
            TextButton(onClick = onDismiss, modifier = Modifier.heightIn(min = 48.dp)) { Text("关闭") }
        },
    )
}

@Composable
private fun QuestionPromptForm(
    attentionId: String,
    question: QuestionPromptUi,
    selectedOptionIds: List<String>,
    freeForm: String,
    enabled: Boolean,
    onOptionToggle: (String) -> Unit,
    onFreeFormChange: (String) -> Unit,
) {
    Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            question.prompt ?: "问题正文不可用",
            modifier = Modifier
                .testTag(questionPromptTestTag(attentionId, question.key))
                .semantics { heading() },
            style = MaterialTheme.typography.bodyLarge,
        )
        question.options.forEach { option ->
            val selected = option.id in selectedOptionIds
            DecisionButton(
                option = if (selected) option.copy(label = "✓ ${option.label}") else option,
                enabled = enabled,
                modifier = Modifier.testTag(
                    questionOptionTestTag(attentionId, question.key, option.id),
                ),
                selected = selected,
                onClick = { onOptionToggle(option.id) },
            )
        }
        if (question.freeFormAllowed) {
            OutlinedTextField(
                value = freeForm,
                onValueChange = onFreeFormChange,
                modifier = Modifier
                    .fillMaxWidth()
                    .testTag(questionFreeFormTestTag(attentionId, question.key)),
                label = { Text("本题自定义回答") },
                minLines = 2,
                maxLines = 6,
                enabled = enabled,
            )
        }
    }
}

@Composable
private fun DecisionButton(
    option: DecisionOptionUi,
    enabled: Boolean,
    modifier: Modifier = Modifier,
    selected: Boolean? = null,
    onClick: () -> Unit,
) {
    val buttonModifier = if (selected == null) {
        modifier
    } else {
        modifier.semantics { this.selected = selected }
    }
    if (option.tone == DecisionToneUi.Positive) {
        Button(
            onClick = onClick,
            enabled = enabled,
            modifier = buttonModifier.fillMaxWidth().heightIn(min = 48.dp),
        ) { Text(option.label) }
    } else {
        OutlinedButton(
            onClick = onClick,
            enabled = enabled,
            modifier = buttonModifier.fillMaxWidth().heightIn(min = 48.dp),
            colors = if (option.tone == DecisionToneUi.Destructive) {
                ButtonDefaults.outlinedButtonColors(contentColor = MaterialTheme.colorScheme.error)
            } else {
                ButtonDefaults.outlinedButtonColors()
            },
        ) { Text(option.label) }
    }
}

@Composable
internal fun CapabilitiesScreen(
    panel: PanelState<RuntimeCapabilitiesUi>,
    onAction: (UiAction) -> Unit,
    modifier: Modifier = Modifier,
) {
    val capabilities = panel.value
    PanelList(
        title = "运行时能力",
        freshness = panel.freshness,
        loading = panel.loading,
        error = panel.error,
        onRefresh = { onAction(UiAction.Refresh) },
        modifier = modifier,
        empty = capabilities == null,
        emptyText = "选择一个运行时后查看能力证据。",
    ) {
        if (capabilities != null) {
            item {
                Column(verticalArrangement = Arrangement.spacedBy(4.dp)) {
                    Text(capabilities.runtimeLabel, style = MaterialTheme.typography.titleMedium)
                    Text("协商于 ${capabilities.negotiatedLabel}", style = MaterialTheme.typography.bodySmall)
                    Text(
                        "能力以运行时证据为准，不按 provider 名称推断。",
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                }
            }
            items(capabilities.entries.size, key = { capabilities.entries[it].id }) { index ->
                CapabilityCard(capabilities.entries[index])
            }
        }
    }
}

@Composable
private fun CapabilityCard(capability: CapabilityUi) {
    val tone = when (capability.state) {
        CapabilityStateUi.Supported -> StatusTone.Positive
        CapabilityStateUi.Unsupported -> StatusTone.Neutral
        CapabilityStateUi.Unavailable, CapabilityStateUi.NotVerified -> StatusTone.Warning
        CapabilityStateUi.UpstreamBlocked -> StatusTone.Error
    }
    Card(Modifier.fillMaxWidth()) {
        Column(Modifier.padding(14.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween) {
                Text(capability.displayName, Modifier.weight(1f), fontWeight = FontWeight.SemiBold)
                StatusPill(capability.state.label(), tone)
            }
            Text(capability.reason, style = MaterialTheme.typography.bodySmall)
            Text("证据：${capability.evidenceLabel}", style = MaterialTheme.typography.labelSmall)
        }
    }
}

private fun AttentionSubjectUi.title(): String = when (this) {
    is AttentionSubjectUi.Approval -> "审批请求"
    is AttentionSubjectUi.Question -> "需要回答"
    is AttentionSubjectUi.ConnectionFault -> "连接故障"
}

private fun CapabilityStateUi.label(): String = when (this) {
    CapabilityStateUi.Supported -> "支持"
    CapabilityStateUi.Unsupported -> "不支持"
    CapabilityStateUi.Unavailable -> "当前连接不可用"
    CapabilityStateUi.NotVerified -> "未验证"
    CapabilityStateUi.UpstreamBlocked -> "上游阻塞"
}
