# PROTOCOL — UACP v0.3

> 状态：**v0.3 合同定稿，2026-08-09**
> 需求真源是 [REQUIREMENTS.md](REQUIREMENTS.md)。本文件在其之下、在任务卡之上。
> 相关决策：[ADR-0009](adr/0009-session-broker.md)、[ADR-0010](adr/0010-canonical-state-and-workflow.md)、
> [ADR-0011](adr/0011-self-hosted-connectivity.md)、[ADR-0012](adr/0012-provider-decode-strategy.md)、
> [ADR-0018](adr/0018-attention-answer-provenance.md)、
> [ADR-0020](adr/0020-projection-cursors-and-mobile-ingress.md)、
> [ADR-0021](adr/0021-r3-lan-security.md)。

本文件定义 UACP（Unified Agent Control Protocol）的**规范化状态、命令、状态转移、
投影、cursor 与能力语义**。它是 `crates/kaleido-proto` 的逐字来源：proto 中出现的每个类型
都必须能在本文件找到定义，本文件定义的每个 v0.3 类型都必须在 proto 中存在。

**关键词**：必须 / 不得 / 应当 / 可以，按 RFC 2119 语义解释。

---

## 0. 这份协议解决什么

移动端需要的不是上游报文列表，而是六件可恢复的事：

1. 有哪些 provider、项目和会话，各自现在是什么状态；
2. 一个会话里发生过什么、正在发生什么；
3. 我发出的输入现在到哪一步了，是排队还是真的注入了当前 turn；
4. 有哪些事在等我处理；
5. 一个跨 Agent 工作流走到哪一步了；
6. 当前这条连接**实际**能做什么。

因此 UACP 的核心是 canonical state + command + projection，而不是固定数量的事件枚举
（[ADR-0010](adr/0010-canonical-state-and-workflow.md) D-1/D-2）。数据路径固定为：

```
provider 报文
  → provider-specific decoder
  → provider reducer
  → StateEffect（canonical 状态转移）
  → durable log（分配 cursor）
  → projection fanout（按 ProjectionKey 重算读模型）
  → projection journal（按 key 分配独立 cursor）
```

其中 Codex app-server decoder 由
[ADR-0012](adr/0012-provider-decode-strategy.md) 单独授权为「钉定 JSON Pointer +
必需面漂移守卫」。该 ADR **不**定义 OpenCode、Claude Code 或 ACP 的解码策略。

---

## 1. 版本与兼容

```
PROTOCOL_VERSION = "0.3.0"
```

- 握手时双方交换 `protocol_version`，格式必须是三个十进制分量
  `major.minor.patch`，不得带缺失分量、前后缀或非数字字符。
- R3 LAN 上的交换顺序、Host SPKI pin、设备认证与 frame 由
  [TRANSPORT 0.1](TRANSPORT.md) 定义：先接受 transport hello，再接受 UACP hello，最后完成
  设备认证；三者全部成功前不得解码业务消息。版本失败是 transport error，不是 provider
  `CanonicalError`。
- v0.3 是 pre-1.0。**minor 是兼容边界**：本实现只接受 `0.3.x`；`0.0.x`、
  `0.1.x`、`0.2.x`、其他 `0.x`、`1.x` 和畸形版本都必须在解码业务消息前拒绝。
- `0.3.x` 的 patch 版本只允许文档澄清和不改变 wire 形状的修复。
- 所有 v0.3 UACP wire enum 都是**闭合合同**。未知 UACP `kind` 必须解码失败，
  不得猜成相近语义，也不得声称新增变体天然向前兼容。
- v0.2 durable data 不做启发式迁移；特别是旧 `Actor::Human { device_label }` 不得按显示名
  铸造 `DeviceId`。遇到旧形状必须 fail-loud，再由显式离线迁移工具或清理策略处理。
- 上游 provider 标签不直接成为 UACP enum。adapter 遇到未知 method、item 类型、
  status 或标签时，按 provider 合同产生 `DiagnosticRecorded` 或
  `RuntimeProtocolViolation`；原始标签只可放进敏感 `ContentRef`，不得进入普通日志。
- 任何破坏性变更必须同时更新本文件、`kaleido-proto`、受影响任务卡与 ADR，并提升
  pre-1.0 minor 版本。

线上编码约定：带数据的枚举使用内部标签，标签键固定为 `kind`，变体名 `snake_case`。
因此**枚举变体内不得出现名为 `kind` 的字段**（例如 `CompletionCondition::ArtifactProduced`
的字段名是 `artifact_kind`）。普通 record 可以有 `kind` 字段。

闭合 enum 清单：`ProviderBindingKind`、`ContentKind`、`Sensitivity`、
`ContentAvailability`、`ContentUnavailableReason`、`HostPlatform`、
`HostReachability`、`ProviderFamily`、`LaunchSurface`、`ConnectionState`、
`ConnectionFaultReason`、`Capability`、`CapabilityState`、
`CapabilityUnavailableReason`、`EvidenceSource`、`OwnershipMode`、
`SessionStatus`、`HistorySourceKind`、`LiveBinding`、`LiveUnboundReason`、
`TurnStatus`、`TurnOrigin`、`ItemStatus`、`ItemBody`、`MessagePhase`、
`ToolSurface`、`FileChangeKind`、`PlanEntryState`、`ItemDiagnosticCode`、
`DiagnosticSeverity`、
`QueueIntent`、`QueueState`、`AttentionSubject`、`AttentionState`、
`AttentionAnswerSource`、`AttentionAnswerEvidenceSource`、
`JoinState`、`JoinFailureReason`、`DecisionSemantics`、`WorkflowState`、
`StepRole`、`StepState`、`CompletionCondition`、`ArtifactKind`,
`WorkflowAction`、`StepBlocker`、`ErrorCode`、`Actor`、`Command`、
`CommandOutcome`、`StateEffect`、`DiagnosticCode`、`StreamKey`、
`SnapshotPayload`、`SubscribeOutcome`、`ProjectionKey`、`ProjectionPayload`、
`ProjectionSubscribeOutcome`、`ContentWriteResponse` 和
`ContentReadResponse`。v0.3 没有用于承接上游未知标签的开放 wire enum。

---

## 2. 设计约束（评审必查）

| ID | 约束 |
|---|---|
| R-P1 | wire 类型必须 UniFFI 可表达：只用具名字段 record、具名字段或无字段 enum、`String`、`bool`、`u8`、`u32`、`i64`、`u64`、`f64`、`Vec<T>`、`Option<T>`。**禁止**泛型、元组、元组结构体、trait object、未定型 JSON、map、`usize`、`u128`、`Duration`、`SystemTime`。validator 的 Rust-only 参数/返回类型不属于 wire 类型 |
| R-P2 | 时间统一为 `i64` Unix epoch 毫秒（字段名以 `_at_ms` 结尾）。禁止在 canonical 中出现本地时区或字符串时间戳 |
| R-P3 | canonical 类型中不得出现上游 discriminator 字面量（A-4 门禁：`item/`、`thread/`、`session/update`、`agent_message_chunk`、`tool_call_update`），**包括文档注释** |
| R-P4 | 每个可见状态必须能由 `Snapshot + 其后的 LogRecord` 重建（[ARCHITECTURE](ARCHITECTURE.md) INV-4） |
| R-P5 | 大载荷与敏感载荷只以 `ContentRef` 出现在 canonical、日志和推送中；正文存内容寻址存储，并只经 §4.10 的鉴权内容查询读取 |
| R-P6 | 能力属于 runtime 连接实例，携带证据；UI 不得按 provider 名称或版本分支（INV-5） |
| R-P7 | `history_source` 与 `live_binding` 是两个独立字段，任一方向都不得互相推断（INV-2） |
| R-P8 | 「审批被拒绝」是 Item 的正常终态，不是错误，也不使 Turn 失败 |
| R-P9 | 用户输入只有在 runtime 明确确认注入活动 turn 后才能标记为 steer 已生效 |
| R-P10 | 命令确认必须区分「Broker 本地已接受」与「runtime 已接受」；包含确认的 `LogRecord.cursor` 是唯一权威 cursor |

R-P8、R-P9、R-P10 各自有一手证据或直接需求来源，见 §11 与 §4.6。

---

## 3. 标识

所有 ID 都是具名字段 record，而不是 tuple struct：

```
HostId { value: String }
DeviceId { value: String }
ProviderRuntimeId { value: String }
ProjectId { value: String }
ProjectBindingId { value: String }
ProviderBindingId { value: String }
SessionId { value: String }
TurnId { value: String }
ItemId { value: String }
AgentTaskId { value: String }
QueueEntryId { value: String }
AttentionId { value: String }
WorkflowId { value: String }
StepId { value: String }
ArtifactId { value: String }
CommandId { value: String }
ContentId { value: String }
BlockerId { value: String }
```

除 `ProviderBindingId` 外，它们都是 Broker 分配的不透明 UTF-8 字符串且不得为空。
`ProviderBindingId` 还必须使用 Broker 命名空间 `bnd_`，后缀至少 8 个 ASCII
字母/数字/`-`/`_`。它不是上游 ID。

canonical 只保存 Broker 分配的绑定句柄：

```
ProviderBindingHandle {
  id: ProviderBindingId,
  runtime_id: ProviderRuntimeId,
  kind: ProviderBindingKind,
}

ProviderBindingKind =
  | Session
  | Turn
  | Item
  | InteractionRequest
  | RuntimeAcknowledgement
```

原始 provider session/turn/item/request ID 只存在于 adapter 私有绑定存储：

```
ProviderBindingId → (runtime, kind, provider-private raw ID)
```

该映射不是 UACP wire 类型，不进入 canonical state、durable log、projection、推送或
普通日志。不得把原始 provider ID 直接填进任何 canonical ID。
`Session.binding_handle`、`Turn.binding_handle`、`Item.binding_handle` 分别只接受
`ProviderBindingKind::Session`、`Turn`、`Item`；kind 不匹配必须拒绝，不能跨实体复用
一个看似合法的 `bnd_` 句柄。`ApprovalRequest` / `QuestionRequest` 只接受
`InteractionRequest`；`SteerAcknowledgement` 与
`CommandOutcome::AcceptedByRuntime` 只接受 `RuntimeAcknowledgement`。

---

## 4. Canonical 对象

层级与 [REQUIREMENTS](REQUIREMENTS.md) §2 一致：

```
Host
└── ProviderRuntime
    └── Project
        ├── Session ── Turn ── Item
        ├── Workflow ── Step
        └── AttentionItem
```

### 4.1 Host / ProviderRuntime / Project

```
Host {
  id: HostId,
  display_name: String,
  platform: HostPlatform,
  reachability: HostReachability,
  protocol_version: String,
  last_seen_at_ms: i64,
}

HostPlatform = Windows | MacOs | Linux

HostReachability = Offline | LanDirect | PeerToPeer | Relayed

ProviderRuntime {
  id: ProviderRuntimeId,
  host_id: HostId,
  family: ProviderFamily,
  version_label: Option<String>,    // 仅用于展示与诊断，禁止用于功能分支（R-P6）
  launch_surface: LaunchSurface,
  connection: ConnectionState,
  capabilities: RuntimeCapabilities,
  binding_handle: Option<ProviderBindingHandle>,
}

ProviderFamily = Codex | ClaudeCode | OpenCode | Acp

LaunchSurface =
  | BrokerLaunched
  | SharedServer
  | ExternalNativeCli
  | ExternalNativeGui

Project {
  id: ProjectId,
  display_name: String,
  bindings: Vec<ProjectBinding>,
  session_counts: SessionCounts,
  workflow_count: u32,
  attention_count: u32,
  last_activity_at_ms: i64,
}

ProjectBinding {
  id: ProjectBindingId,
  project_id: ProjectId,
  runtime_id: ProviderRuntimeId,
  root_ref: ContentRef,
}

SessionCounts { total: u32, running: u32, waiting_human: u32, failed: u32, archived: u32 }

ConnectionState =
  | Disconnected
  | Connecting
  | Connected { since_at_ms: i64 }
  | Degraded { reason: ConnectionFaultReason, since_at_ms: i64 }
  | Unavailable { reason: ConnectionFaultReason, since_at_ms: i64 }

ConnectionFaultReason =
  | ProcessExited { exit_code: Option<i64> }
  | HandshakeRejected
  | AuthRequired
  | Timeout
  | TransportError
  | ProtocolViolation
```

`ProviderFamily` 存在**仅**为分类展示（REQUIREMENTS §2.1 要求按 provider 分类）。
任何功能可用性判断必须读 `capabilities`。

`Project` 是逻辑项目，不属于单个 runtime。`bindings` 表达它在不同 provider/runtime
上的实际落点；同一 Project 内 `ProjectBinding.id` 不得重复，且每个 binding 的
`project_id` 必须等于外层 `Project.id`。`root_ref` 是完整路径，必须为
`Sensitivity::Sensitive` 且没有 preview。Session 与 Workflow Step 必须引用实际
`ProjectBindingId`，不能只引用逻辑 Project。

### 4.2 能力

```
RuntimeCapabilities {
  runtime_id: ProviderRuntimeId,
  negotiated_at_ms: i64,
  entries: Vec<CapabilityEntry>,
}

CapabilityEntry {
  capability: Capability,
  state: CapabilityState,
  evidence: CapabilityEvidence,
}

Capability =
  | HistoryList | HistoryRead | HistoryResume
  | LiveObserve | LiveControl | LiveMultiSubscriber
  | TurnPrompt | TurnSteer | TurnInterrupt | TurnRetry
  | InteractionApproval | InteractionQuestion
  | StatePlan | StateTasks | StateDiff | StateToolLifecycle
  | QueueRead | QueueWrite | QueueReorder
  | WorkflowParticipate

CapabilityState =
  | Supported
  | Unsupported
  | UnavailableOnThisConnection { reason: CapabilityUnavailableReason }
  | NotVerified
  | UpstreamBlocked { blocker_id: BlockerId }

CapabilityUnavailableReason =
  | RuntimeDisconnected
  | AuthenticationRequired
  | SubscriptionLost
  | PolicyRestricted
  | TemporarilyUnavailable

CapabilityEvidence {
  source: EvidenceSource,
  observed_at_ms: i64,
  note_ref: Option<ContentRef>,
}

EvidenceSource =
  | HandshakeDeclared
  | ObservedInTraffic
  | RecordedFixture
  | ManualAcceptance
  | Absent
```

五个 `CapabilityState` 精确对应 [ADR-0009](adr/0009-session-broker.md) D-4 要求 UI 必须区分的五种情形。
`UpstreamBlocked` 是 REQUIREMENTS §8 六格验收里「上游阻塞」的协议表达；它**不是通过**。

未在 `entries` 中出现的能力，读模型必须按 `NotVerified` 渲染，不得按 `Unsupported`
或 `Supported` 假定。

同一个 `RuntimeCapabilities` 中同一 `Capability` 不得重复。`note_ref` 若存在，
必须为无 preview 的 Sensitive `ContentRef`；连接错误的自由文本原文不得塞进
`UnavailableOnThisConnection`。

`LiveControl` 是具体控制能力之上的已取得证据：只有当前 live connection 上、与本地
`CommandEnvelope.command_id` 关联的状态改变命令产生
`CommandOutcome::AcceptedByRuntime` 后，才能以 `ObservedInTraffic` 证明它。`AcceptedLocally`、
`Enqueued`、transport 写入成功、provider 名称/版本、handshake 声明和 fixture replay 均不足以
证明 `LiveControl`。`LiveControl::Supported` 不推出 `TurnPrompt`、`TurnSteer`、
`InteractionApproval` 或其他具体能力为 Supported。

### 4.3 Session

```
Session {
  id: SessionId,
  project_id: ProjectId,
  project_binding_id: ProjectBindingId,
  ownership: OwnershipMode,
  history_source: HistorySource,
  live_binding: LiveBinding,
  status: SessionStatus,
  title: Option<String>,
  created_at_ms: i64,
  updated_at_ms: i64,
  last_activity_at_ms: i64,
  active_turn_id: Option<TurnId>,
  queue_depth: u32,
  open_attention_count: u32,
  archived: bool,
  binding_handle: Option<ProviderBindingHandle>,
}

OwnershipMode = BrokerManaged | SharedRuntime | ExternalNative

SessionStatus =
  | Offline | Idle | Running | WaitingUser | WaitingApproval
  | Queued | Failed | Completed | Cancelled

HistorySource {
  kind: HistorySourceKind,
  runtime_id: Option<ProviderRuntimeId>,
  evidence: CapabilityEvidence,
}

HistorySourceKind = None | ProviderApi | ProviderLocalStore | BrokerLog

LiveBinding =
  | NotBound { reason: LiveUnboundReason }
  | Observing { runtime_id: ProviderRuntimeId, since_at_ms: i64,
                evidence: CapabilityEvidence }
  | Controlling { runtime_id: ProviderRuntimeId, since_at_ms: i64,
                  evidence: CapabilityEvidence }
  | Blocked { blocker_id: BlockerId }

LiveUnboundReason =
  | NeverStarted | RuntimeExited | SubscriptionLost
  | NoPublicAttachPath
```

规范：

- `Observing` 只允许在实际收到该 runtime 关于**这个** session 的实时报文后置位。
  `evidence.source` 必须是 `ObservedInTraffic`。能列出或恢复历史，永远不足以置
  `Observing`（R-P7、INV-2）。
- `Controlling` 要求同时具备 `LiveObserve` 与 `LiveControl`，并且这个 Session 在当前
  live binding 上已有至少一个本地状态改变命令得到 `AcceptedByRuntime`。同 runtime 的另一个
  Session 不得借用该证据。`since_at_ms` 保留 live binding 建立时间，
  `evidence.observed_at_ms` 是首次 runtime 接受控制的时间。
- write path 不得仅凭两个 capability 接受 `Controlling`。当前 `SubmitPrompt` 纵切还必须能
  回查到同一 Session、同一 runtime 的唯一 `TurnOrigin::RemoteCommand { command_id }` 和该
  command 的 `AcceptedByRuntime`；另一个 Session 或 runtime 的证据不得借用。
- `Observing` / `Controlling` 的 `runtime_id` 必须等于用于验证的
  `RuntimeCapabilities.runtime_id`。
- 写入 `Observing` / `Controlling` 时，候选 Session 必须从它自己的 binding 或 history
  runtime 引用解析到当前 runtime；不得在候选对象删除或改写引用后，借用 store 中旧 Session
  的 runtime 或 capability 通过校验。
- `BrokerManaged`、`SharedRuntime` 与 `ExternalNative` 使用相同的控制证据门槛；ownership
  不构成能力证据。Replay、仅观察、无本地 command correlation 或只有本地接受的路径不得进入
  `Controlling`。
- R3 客户端显示一个具体干预按钮时，必须同时检查 Session 仍为 live，以及该动作对应的具体
  capability 为 `Supported`。`Controlling` 表示已取得的 Session 级控制证据，不是所有按钮的
  总开关；特别地，它不得顺带提升 `TurnSteer`。
- 读磁盘 transcript、扫描进程、猜窗口标题得到的信息，只能进入 `HistorySource`，
  **不得**进入 `LiveBinding`。
- `SessionStatus::Queued` 的精确含义是：没有活动 turn，且输入队列中至少有一条
  `Pending` 条目等待提交。它不表示「已经在跑」。
- `SessionStatus::WaitingApproval` 与 `WaitingUser` 分别对应存在 `Open` 状态的
  `AttentionSubject::Approval` 与 `AttentionSubject::Question`。两者同时存在时取
  `WaitingApproval`。
- `Offline` 表示 host 或 runtime 不可达，**必须**诚实显示（REQUIREMENTS §6）。

### 4.4 Turn

```
Turn {
  id: TurnId,
  session_id: SessionId,
  status: TurnStatus,
  origin: TurnOrigin,
  started_at_ms: Option<i64>,
  completed_at_ms: Option<i64>,
  item_ids: Vec<ItemId>,
  error: Option<CanonicalError>,
  binding_handle: Option<ProviderBindingHandle>,
}

TurnStatus = Pending | Running | AwaitingInteraction | Completed | Failed | Cancelled

TurnOrigin =
  | LocalSurface
  | RemoteCommand { command_id: CommandId }
  | WorkflowStep { step_id: StepId }
```

规范：

- `Turn.item_ids` 是 Broker 观测到的**完整**顺序列表，必须由逐条 item 转移累积得到。
  **不得**用上游「turn 结束」报文里的摘要列表替换它（§11 记录了该陷阱的一手证据）。
- `Turn.error` 只在 `Failed` 时非空。被拒绝的审批不写入这里（R-P8）。
- `AwaitingInteraction` 表示 turn 仍在进行，但当前被一个 `Open` 的 AttentionItem 挡住。
- `RemoteCommand.command_id` 必须是触发这个 Turn 的真实 `CommandEnvelope.command_id`。对于
  Codex `turn/start`，只有显式关联到该 command 的 response 才能设置这个 origin；replay 或
  未关联流量不得反推成本地命令来源。
- 一个 `RemoteCommand.command_id` 只能绑定一个 Turn。Turn 建立后，后续 upsert 不得改写其
  `session_id`、`origin` 或已存在的 provider binding identity；冲突必须在追加日志前拒绝。

### 4.5 Item

```
Item {
  id: ItemId,
  session_id: SessionId,
  turn_id: TurnId,
  sequence: u64,                    // 会话内单调，供确定性排序
  status: ItemStatus,
  body: ItemBody,
  created_at_ms: i64,
  updated_at_ms: i64,
  binding_handle: Option<ProviderBindingHandle>,
}

ItemStatus = Pending | InProgress | Completed | Declined | Failed | Cancelled

ItemBody =
  | UserMessage   { content: ContentRef }
  | AgentMessage  { content: ContentRef, phase: MessagePhase }
  | Reasoning     { content: ContentRef }
  | ToolCall      { tool: ToolDescriptor, arguments: Option<ContentRef>,
                    output: Option<ContentRef>, exit_code: Option<i64> }
  | FileEdit      { change_set: ChangeSet }
  | PlanUpdate    { entries: Vec<PlanEntry> }
  | TaskUpdate    { tasks: Vec<AgentTask> }
  | Diagnostic    { severity: DiagnosticSeverity,
                    code: ItemDiagnosticCode,
                    detail: ContentRef }

MessagePhase = Commentary | FinalAnswer

ToolDescriptor { name: String, surface: ToolSurface }
ToolSurface = ShellCommand | FileSystem | McpServer { server_name: String }
            | Builtin

ChangeSet { entries: Vec<FileChange>, truncated: bool }
FileChange { path_ref: ContentRef, kind: FileChangeKind, diff: Option<ContentRef> }
FileChangeKind = Add | Modify | Delete | Rename { from_ref: ContentRef }

PlanEntry { title_ref: ContentRef, state: PlanEntryState }
PlanEntryState = Pending | InProgress | Completed | Skipped

AgentTask { id: AgentTaskId, title_ref: ContentRef, state: PlanEntryState }

DiagnosticSeverity = Info | Warning | Error

ItemDiagnosticCode =
  | RuntimeNotice
  | UnsupportedContent
  | ContentUnavailable
  | ValidationFailure
```

规范：

- `ItemStatus::Declined` 是终态，语义是「人工拒绝了这个操作」。它**不得**被映射为
  `Failed`，也不得使所属 Turn 变为 `Failed`（R-P8）。
- 上游未知 item 类型不得猜成 `AgentMessage` 或其他已知 `ItemBody`；adapter 产生
  `DiagnosticRecorded { code: UnknownUpstreamLabel }`，需要展示原始细节时只放入
  Sensitive `ContentRef`。
- `ToolCall.arguments`、`ToolCall.output`、`FileChange.diff`、`FileChange.path_ref`
  一律是 `ContentRef`，且 `sensitivity = Sensitive`（§10）。
- User/Agent/Reasoning 正文、plan/task 标题、item diagnostic detail 也必须是
  `Sensitivity::Sensitive` 且 `preview = None`。
- 流式增量不产生新 Item：它更新既有 Item 的 `content` 与 `updated_at_ms`。

### 4.6 用户输入队列

这是 [ADR-0010](adr/0010-canonical-state-and-workflow.md) D-3 的第三类状态，必须与运行状态和 Agent 任务分离。

```
QueueEntry {
  id: QueueEntryId,
  session_id: SessionId,
  position: u32,                    // 0 起，Pending 条目内连续
  intent: QueueIntent,              // NewTurn | SteerActiveTurn
  body: ContentRef,
  state: QueueState,
  editable: bool,
  created_at_ms: i64,
  updated_at_ms: i64,
}

QueueState =
  | Pending
  | Submitting { command_id: CommandId }
  | DeliveredAsNewTurn { turn_id: TurnId, delivered_at_ms: i64 }
  | DeliveredAsSteer   { turn_id: TurnId, runtime_id: ProviderRuntimeId,
                          binding_handle: ProviderBindingHandle,
                          injected_at_ms: i64,
                          ack: SteerAcknowledgement }
  | Rejected { error: CanonicalError }
  | Cancelled { at_ms: i64 }

SteerAcknowledgement {
  source: EvidenceSource,           // 必须是 ObservedInTraffic
  runtime_id: ProviderRuntimeId,
  session_id: SessionId,
  turn_id: TurnId,
  binding_handle: ProviderBindingHandle,
  observed_at_ms: i64,
}
```

规范（R-P9，直接来自 REQUIREMENTS §2.2 与 ARCHITECTURE §10）：

- 只有当 runtime 返回**指向当前活动 turn 的注入确认**时，条目才能进入
  `DeliveredAsSteer`；`SteerAcknowledgement.source` 必须是 `ObservedInTraffic`。
- 如果 `TurnSteer` 能力不是 `Supported`，`SteerActiveTurn` 意图的条目**必须**留在
  `Pending`，并在投影中显示为排队。把它显示成「已引导」是打回项。
- `Pending` 与 `Submitting` 都不得显示为「已送达」。
- `editable` 仅在 `Pending` 时为 `true`。
- `DeliveredAsSteer` 外层与 acknowledgement 的 runtime、turn、binding 必须逐字一致，
  acknowledgement 的 session 必须等于 `QueueEntry.session_id`；写路径还必须以当前
  `active_turn_id` 和该 runtime 的 `TurnSteer = Supported` 验证。
- `ReorderQueue` 的 `order` 必须**恰好**包含目标 session 的全部 `Pending` 条目：
  无遗漏、无重复、无未知 ID、无其他 session 条目、无非 `Pending` 条目。任一条件
  不满足时整个命令以 `InvalidCommand` 拒绝，不得部分重排。

### 4.7 Attention Inbox

第四类状态，聚合一切等待人工的事项（ADR-0010 D-3、ARCHITECTURE §4.2）。

```
AttentionItem {
  id: AttentionId,
  host_id: HostId,
  project_id: ProjectId,
  session_id: Option<SessionId>,
  turn_id: Option<TurnId>,
  workflow_id: Option<WorkflowId>,
  subject: AttentionSubject,
  state: AttentionState,
  created_at_ms: i64,
  expires_at_ms: Option<i64>,
}

AttentionSubject =
  | Approval        { request: ApprovalRequest }
  | Question        { request: QuestionRequest }
  | WorkflowGate    { request: WorkflowGateRequest }
  | ConnectionFault { runtime_id: ProviderRuntimeId, reason: ConnectionFaultReason }

AttentionState =
  | Open
  | Answered { option_id: Option<String>, free_form_ref: Option<ContentRef>,
               decided_at_ms: i64, answer_source: AttentionAnswerSource }
  | Expired  { at_ms: i64 }
  | Superseded { by: AttentionId }
  | Cancelled { at_ms: i64 }

AttentionAnswerSource =
  | LocalCommand { command_id: CommandId }
  | ObservedExternal { evidence: AttentionAnswerEvidence }

AttentionAnswerEvidence {
  observer_host_id: HostId,
  observed_at_ms: i64,
  source: AttentionAnswerEvidenceSource,
}

AttentionAnswerEvidenceSource =
  | ObservedInTraffic
  | RecordedFixture

ApprovalRequest {
  request_key: String,              // Broker 分配，跨重连稳定
  target_item_id: ItemId,
  join: JoinState,
  options: Vec<DecisionOption>,
  summary_ref: ContentRef,
  detail_ref: Option<ContentRef>,
  binding_handle: ProviderBindingHandle,
}

JoinState =
  | Joined   { item_id: ItemId }
  | Unjoined { reason: JoinFailureReason }

JoinFailureReason = ItemNotYetSeen | ItemUnknown | AmbiguousTarget
                  | ScopeMismatch

QuestionRequest {
  request_key: String,
  prompt_ref: ContentRef,
  options: Vec<DecisionOption>,
  free_form_allowed: bool,
  binding_handle: ProviderBindingHandle,
}

WorkflowGateRequest {
  request_key: String,
  step_id: StepId,
  prompt_ref: ContentRef,
  options: Vec<DecisionOption>,
  free_form_allowed: bool,
}

DecisionOption {
  option_id: String,
  label: String,
  semantics: DecisionSemantics,
}

DecisionSemantics =
  | Allow
  | AllowAlways
  | Deny
  | DenyAlways
  | Cancel
  | Choose

AttentionResponse {
  attention_id: AttentionId,
  session_id: Option<SessionId>,
  request_key: String,
  expected_expires_at_ms: Option<i64>,
  option_id: Option<String>,
  free_form_ref: Option<ContentRef>,
}
```

规范：

- 审批请求的上游载荷可能只带一个 item 引用而不带可展示上下文，因此 `join` 是必填字段，
  且 `Unjoined` 必须能渲染（一手证据见 §11）。
- `ApprovalRequest.target_item_id` 保留 Broker canonical ItemId，使延迟 join 可重试。
  尚未见到 item 时为 `ItemNotYetSeen`；观察窗口结束仍未见到为 `ItemUnknown`；
  重复目标为 `AmbiguousTarget`；session/turn 不同为 `ScopeMismatch`。
- Approval 与 Question 必须有 `AttentionItem.session_id`。WorkflowGate 必须有
  `workflow_id`、稳定 `request_key`、可选择的 options 或允许 free-form，因此三类都
  可以被同一个 `AttentionResponse` 回答。
- 回复必须同时绑定 `attention_id`、目标 `session_id`（若有）、`request_key` 与
  `expected_expires_at_ms`；expected expiry 必须与当前 AttentionItem 完全一致。
  validator 还必须检查当前 state 是 `Open`、`now_ms < expires_at_ms`、选择项存在，
  以及 free-form 是否允许。
- 本地 `RespondAttention` 接受后，`answer_source` 必须是
  `LocalCommand { command_id }`，且 `command_id` 必须是实际进入 Broker 的
  `CommandEnvelope.command_id`；不得为上游决定铸造 ID。
- 无法关联到本地命令的 live reply 必须使用
  `ObservedExternal { evidence.source = ObservedInTraffic }`；真实 fixture replay 必须使用
  `RecordedFixture`。`observer_host_id` 必须非空且等于外层 `AttentionItem.host_id`；
  replay 的 `observed_at_ms` 必须来自原始记录，不得使用 replay 墙钟。
- `ObservedExternal` 只证明 Broker 在何处、何时、通过何种媒介观察到决定，不声明外部 actor 身份。
- 过期后的回复必须以 `ErrorCode::ApprovalExpired` 拒绝。重复提交同一个本地幂等键返回
  `CommandOutcome::Duplicate`；对任何已经 `Answered` 的事项新发本地回复，均返回
  `Rejected { error.code = ApprovalAlreadyAnswered }`，不得下发给 runtime。
- `options` 由 runtime 提供，**不得**在客户端硬编码为「同意/拒绝」两项。
- 同一 request 的 `option_id` 不得重复；回复至少要有 option 或 free-form 之一。
- `ConnectionFault` 使会话在移动端 Inbox 可见，但不产生 `Turn.error`。

### 4.8 Workflow

Workflow 是 v1 必做（[ADR-0010](adr/0010-canonical-state-and-workflow.md) D-4），v0.3 只定义状态与人工推进，不含自动调度策略。

```
Workflow {
  id: WorkflowId,
  project_id: ProjectId,
  title: String,
  state: WorkflowState,
  step_ids: Vec<StepId>,
  created_at_ms: i64,
  updated_at_ms: i64,
}

WorkflowState = Draft | Ready | Running | Blocked | WaitingHuman
              | Review | Rework | Completed | Failed | Cancelled

Step {
  id: StepId,
  workflow_id: WorkflowId,
  title: String,
  role: StepRole,
  assignment: StepAssignment,
  depends_on: Vec<StepId>,
  inputs: Vec<ArtifactId>,
  outputs: Vec<ArtifactId>,
  completion: CompletionCondition,
  human_gate: Option<AttentionId>,
  session_id: Option<SessionId>,
  state: StepState,
  attempt: u32,
  audit: Vec<StepTransition>,
}

StepState = Draft | Ready | Running | Blocked | WaitingHuman
          | Review | Rework | Completed | Failed | Skipped | Cancelled

StepRole = Plan | Implement | Review | Verify | Custom

StepAssignment {
  selector: RuntimeSelector,
  project_binding_id: ProjectBindingId,
  worktree_ref: ContentRef,
}

RuntimeSelector {
  family: ProviderFamily,           // 仅表达用户意图
  required: Vec<Capability>,        // 实际调度依据（R-P6）
  runtime_id: Option<ProviderRuntimeId>,
}

CompletionCondition =
  | AgentTurnCompleted
  | ArtifactProduced { artifact_kind: ArtifactKind }
  | HumanApproved

Artifact {
  id: ArtifactId,
  workflow_id: WorkflowId,
  produced_by: Option<StepId>,
  kind: ArtifactKind,
  content: ContentRef,
  created_at_ms: i64,
}

ArtifactKind = Plan | Diff | Commit | ReviewNotes | TestReport

StepTransition {
  from: StepState,
  to: StepState,
  action: WorkflowAction,
  actor: Actor,
  at_ms: i64,
  reason_ref: Option<ContentRef>,
}

WorkflowAction = Advance | Retry | Rework | Skip | Cancel | Reassign

StepBlocker =
  | DependencyIncomplete { step_id: StepId }
  | CapabilityNotSupported { capability: Capability }
  | HumanGateOpen { attention_id: AttentionId }
  | NotSchedulable { state: StepState }
```

规范：

- Step 只有在 `depends_on` 全部为 `Completed` 或 `Skipped`、
  `assignment.selector.required` 的能力全为 `Supported`、且 `human_gate` 已
  `Answered` 时才可进入 `Running`。
- `assignment.project_binding_id` 必须属于外层 `Workflow.project_id`；
  `worktree_ref` 是实际 worktree 的完整路径，必须为 Sensitive 且无 preview。
  因此一个 Workflow 可以在同一逻辑 Project 下跨 provider/runtime binding。
- `reason_ref` 若存在必须是 Sensitive ContentRef；不得把返工原因、blocker 文本或
  工具参数写成普通 `String` 进入 canonical/log。
- `attempt` 只能 checked increment；`u32::MAX` 后 retry 必须明确失败，不能饱和。

### 4.8.1 Workflow 人工动作与允许转移

| 动作 | 允许转移 |
|---|---|
| `Advance` | `Draft→Ready`、`Ready→Running`、`Running→Review`、`Running→Completed`、`WaitingHuman→Ready`、`Blocked→Ready`、`Rework→Ready`、`Review→Completed` |
| `Retry` | `Failed→Ready`，并 checked increment `attempt` |
| `Rework` | `Review→Rework`、`Completed→Rework`、`Failed→Rework` |
| `Skip` | `Draft/Ready/Blocked/WaitingHuman/Review/Rework/Failed→Skipped`；Running 必须先取消/中断 |
| `Cancel` | 任意非终态 Step → `Cancelled` |
| `Reassign` | `Draft/Ready/Blocked/WaitingHuman/Rework/Failed` 中保持原 state，并替换 `StepAssignment` |

不在表内的转移必须以 `InvalidCommand` 拒绝。`CancelWorkflow` 把非终态 Workflow
转为 `Cancelled`；它不允许把已完成 Workflow 改写。所有动作写入 `StepTransition`。

### 4.9 内容引用

```
ContentRef {
  content_id: ContentId,
  kind: ContentKind,
  byte_len: u64,
  digest: String,
  preview: Option<String>,
  sensitivity: Sensitivity,
  availability: ContentAvailability,
}

ContentKind = PlainText | Markdown | ToolArguments | ToolOutput
             | UnifiedDiff | FilePath | StructuredSummary

Sensitivity = Business | Sensitive

ContentAvailability = Inline | Stored | Evicted | NeverStored
```

规范（R-P5）：

- `sensitivity = Sensitive` 的内容：`preview` 必须为 `None`，正文不得写入普通日志与推送。
- `availability = Inline` 只允许 `byte_len ≤ 4096` 且 `sensitivity = Business`。
- `digest` 必须严格是 `sha256:` 加 64 个小写十六进制字符。
- Business preview 最多 256 UTF-8 bytes，且不得含控制符、`/`、`\`、授权头或常见
  token 前缀；否则必须改为 Sensitive/no-preview。
- 正文按 `content_id` 存内容寻址存储；`digest` 是完整性校验依据。`ContentRef`
  永远只是元数据，即使 availability 是 `Inline`，正文也不得嵌入 durable log。
- `Evicted` 表示按保留策略已删除正文；投影必须显示为「内容已过期」，不得显示为空文本。
- 文件路径一律是 `ContentRef`（`ContentKind::FilePath`），因为完整路径含用户名（§10）。

### 4.10 内容读取

手机通过独立的鉴权、加密内容查询读取 `ContentRef` 正文。它不是 `Command`，
不是 `StateEffect`，也不写 durable log、projection、push 或 relay 元数据。

```
ContentReadRequest {
  content_id: ContentId,
  offset: u64,
  max_bytes: u32,
}

ContentReadChunk {
  content_id: ContentId,
  offset: u64,
  bytes: Vec<u8>,
  next_offset: Option<u64>,
  eof: bool,
  digest: String,
}

ContentReadResponse =
  | Chunk { chunk: ContentReadChunk }
  | Unavailable { content_id: ContentId, reason: ContentUnavailableReason }

ContentUnavailableReason =
  | Evicted
  | NeverStored
  | NotFound
  | Unauthorized
  | DigestMismatch
```

`max_bytes` 必须在 `1..=65536`，响应 chunk 也不得超过 65536 bytes。每个 chunk 都按
原 ContentRef 的 digest 验证；`next_offset` 必须等于
`offset.checked_add(bytes.len)`。`eof = true` 时它必须为 `None`，否则必须为上述
`Some(next_offset)`；overflow 或不一致都明确拒绝。`Evicted`/`NeverStored` 必须显式
返回，不得以空 bytes 冒充成功。transport 在发送 bytes 前必须完成 Host pin、设备鉴权与
TLS 1.3 加密。R3 的读取请求绑定当前已认证 `DeviceId`；更细粒度的多用户/团队 ACL 不属于
v0.3。

### 4.11 内容写入

手机写入 prompt、free-form answer 或 reason 正文时，先通过独立的鉴权内容写入取得
`ContentRef`，再把该引用放入 `Command`。写入操作不是 `Command`、`StateEffect`、durable
log 或 projection。

```
ContentWriteRequest {
  content_kind: ContentKind,
  byte_len: u64,
  digest: String,
}

ContentWriteResponse =
  | Stored   { content_ref: ContentRef }
  | Rejected { error: CanonicalError }
```

正文不内嵌在 JSON record 中。`ContentWriteRequest` 是控制头；同一个 transport request ID
关联唯一 binary content frame。host 必须在保存前对实际 bytes 重新计算长度与 SHA-256，
不能信任客户端声明。

规范：

- `content_kind` 只允许 `PlainText` 或 `Markdown`；`byte_len` 必须在 `1..=65536`；
- `digest` 必须严格是 `sha256:` 加 64 个小写十六进制字符，并与实际 bytes 一致；长度或
  digest 不一致时返回 `Rejected { error.code = InvalidCommand }`，不得留下正文或元数据；
- `Stored.content_ref` 的 kind、byte_len 与 digest 必须和已验证请求一致，并且必须为
  `sensitivity = Sensitive`、`preview = None`、`availability = Stored`；客户端没有声明或
  降低 sensitivity、添加 preview 的入口；
- host 根据实际 bytes 自行计算 `ContentId`。相同内容可以命中已有内容寻址对象，但响应仍
  必须满足上述完整性约束；
- 上传、后续 ContentRead 与引用该内容的 DeviceCommandRequest 必须绑定同一已认证
  `DeviceId`。孤儿上传受 TTL 与每设备配额约束；
- 正文、digest 以外的内容指纹和 binary frame 不得进入普通 tracing、push 或 relay metadata。

---

## 5. 状态转移与持久日志

### 5.1 StateEffect

`StateEffect` 是**唯一**允许改变 canonical state 的东西。它是状态转移，不是上游事件改名。

```
StateEffect =
  | HostUpserted         { host: Host }
  | RuntimeUpserted      { runtime: ProviderRuntime }
  | CapabilitiesUpdated  { capabilities: RuntimeCapabilities }
  | ProjectUpserted      { project: Project }
  | SessionUpserted      { session: Session }
  | SessionStatusChanged { session_id: SessionId, status: SessionStatus }
  | TurnUpserted         { turn: Turn }
  | ItemUpserted         { item: Item }
  | QueueEntryUpserted   { entry: QueueEntry }
  | QueueReordered       { session_id: SessionId, order: Vec<QueueEntryId> }
  | AttentionUpserted    { item: AttentionItem }
  | WorkflowUpserted     { workflow: Workflow }
  | StepUpserted         { step: Step }
  | ArtifactUpserted     { artifact: Artifact }
  | CommandAcknowledged  { ack: CommandAck }
  | DiagnosticRecorded   { diagnostic: DiagnosticRecord }
```

```
DiagnosticRecord {
  runtime_id: Option<ProviderRuntimeId>,
  session_id: Option<SessionId>,
  code: DiagnosticCode,
  count: u64,
  first_at_ms: i64,
  last_at_ms: i64,
  detail_ref: Option<ContentRef>,
}

DiagnosticCode =
  | UnknownUpstreamMessage
  | UnknownUpstreamLabel
  | PointerResolutionFailed
  | JoinDeferred
  | JoinFailed
  | BackpressureCoalesced
  | MalformedProviderMessage
```

`DiagnosticRecorded` 是 [ADR-0012](adr/0012-provider-decode-strategy.md) D-3 的落点：未知报文既不 panic，也不伪装成已支持投影。

### 5.2 流与 cursor

```
StreamKey = Host { host_id } | Project { project_id }
          | Session { session_id } | Workflow { workflow_id }

Cursor { seq: u64 }                 // 每个 StreamKey 内严格单调递增，步长为 1

LogRecord {
  cursor: Cursor,
  stream: StreamKey,
  appended_at_ms: i64,
  effect: StateEffect,
}
```

规范：

- 同一 `StreamKey` 内 `seq` 连续，不得跳号。跳号即视为日志损坏。
- `Cursor` 只在 durable append 成功后分配（写路径见 ARCHITECTURE §6）。
- 跨流不保证顺序；需要跨流一致的读模型必须自己按 `appended_at_ms` 与显式引用重建。
- `Cursor::next` 必须 checked。当前 cursor 为 `u64::MAX` 时，任何继续 append/replay
  都返回明确 `CursorOverflow`/日志错误；不得饱和、回绕或重复 `u64::MAX`。
- 重复 cursor 与 gap 是不同诊断，但二者都强制重取快照。

### 5.3 快照

```
SnapshotEnvelope {
  stream: StreamKey,
  cursor: Cursor,
  payload: SnapshotPayload,
}

SnapshotPayload =
  | Host { snapshot: HostSnapshot }
  | Project { snapshot: ProjectSnapshot }
  | Session { snapshot: SessionSnapshot }
  | Workflow { snapshot: WorkflowSnapshot }

HostSnapshot {
  host: Host,
  runtimes: Vec<ProviderRuntime>,
  projects: Vec<Project>,
}

ProjectSnapshot {
  project: Project,
  sessions: Vec<Session>,
  workflows: Vec<Workflow>,
  attention: Vec<AttentionItem>,
}

SessionSnapshot {
  session: Session,
  turns: Vec<Turn>,
  items: Vec<Item>,
  queue: Vec<QueueEntry>,
  attention: Vec<AttentionItem>,
  capabilities: RuntimeCapabilities,
}

WorkflowSnapshot {
  workflow: Workflow,
  steps: Vec<Step>,
  artifacts: Vec<Artifact>,
  attention: Vec<AttentionItem>,
}
```

- `SnapshotEnvelope.stream` 必须与 payload 类型及根 ID 一致：Host/Project/Session/
  Workflow 四种流各有同型 snapshot，任何错配都拒绝。
- 快照必须自洽：其中所有引用（project binding、`active_turn_id`、`item_ids`、
  queue session、attention scope、step/artifact、approval join）都能在同一快照内
  解析，或明确标为 `Unjoined` / `Evicted`。
- `SnapshotEnvelope.cursor` 是快照唯一权威 cursor；四个 payload 不复制第二个 cursor。
- 快照压缩不得改变「快照 + 后续日志 = 当前状态」这一等式（R-P4）。
- replay window 的第一条记录必须是 `snapshot.cursor.checked_next()`，其余严格连续且
  全部属于同一 stream。四种 stream 使用同一个验证规则。

### 5.4 收敛口径

验收状态一致性时，判据是：

> 同一命令序列与同一上游报文序列，必须收敛到**相同的 canonical state**，
> 且 cursor 无缺口、无重复应用。

**不得**使用「重连前后字节逐字相同」作为验收标准（ARCHITECTURE §6）。
`appended_at_ms`、`preview` 截断和诊断计数允许不同。

---

## 6. 命令

```
CommandEnvelope {
  command_id: CommandId,
  idempotency_key: String,          // 客户端生成，(actor, key) 唯一
  actor: Actor,
  issued_at_ms: i64,
  expires_at_ms: Option<i64>,
  body: Command,
}

Actor = Human { device_id: DeviceId } | Workflow { workflow_id: WorkflowId } | Broker

DeviceCommandRequest {
  idempotency_key: String,
  ttl_ms: Option<u64>,
  body: Command,
}

Command =
  | SubmitPrompt      { session_id: SessionId, body: ContentRef }
  | EnqueueInput      { session_id: SessionId, body: ContentRef, intent: QueueIntent }
  | EditQueueEntry    { entry_id: QueueEntryId, body: ContentRef }
  | ReorderQueue      { session_id: SessionId, order: Vec<QueueEntryId> }
  | CancelQueueEntry  { entry_id: QueueEntryId }
  | InterruptTurn     { session_id: SessionId, turn_id: TurnId }
  | RetryTurn         { session_id: SessionId, turn_id: TurnId }
  | RespondAttention  { response: AttentionResponse }
  | OpenSession       { project_id: ProjectId, runtime_id: ProviderRuntimeId }
  | ResumeSession     { session_id: SessionId }
  | CloseSession      { session_id: SessionId }
  | AdvanceStep       { step_id: StepId }
  | RetryStep         { step_id: StepId }
  | ReworkStep        { step_id: StepId, reason_ref: Option<ContentRef> }
  | SkipStep          { step_id: StepId, reason_ref: Option<ContentRef> }
  | CancelStep        { step_id: StepId }
  | ReassignStep      { step_id: StepId, assignment: StepAssignment }
  | CancelWorkflow    { workflow_id: WorkflowId }
```

注意 v0.3 **没有** `SteerActiveTurn` 命令。引导一律通过
`EnqueueInput { intent: SteerActiveTurn }` 表达，由 Broker 依据能力和 runtime 确认决定
它最终是 `DeliveredAsSteer` 还是留在 `Pending`。这样协议层面就不存在「假装已引导」的
表达方式（R-P9）。

`DeviceCommandRequest` 是已认证移动连接唯一允许提交的命令入口。它在类型上没有
`Actor`、`CommandId`、`issued_at_ms` 或 `expires_at_ms`，因此远端不能声明 Broker/Workflow
身份或伪造 canonical ID/时间。hostd 必须：

1. 从连接设备目录取得可信 `DeviceId`，注入 `Actor::Human { device_id }`；
2. 分配新的 `CommandId`，以 host 当前时间写入 `issued_at_ms`；
3. 要求 `idempotency_key` 为 `1..=128` UTF-8 bytes；若有 `ttl_ms`，它必须在
   `1..=300000`，并用 checked addition 计算 `expires_at_ms`；越界或 overflow 必须以
   `InvalidCommand` 拒绝；
4. 在 canonical 命令进入 write path 前持久化规范化请求摘要。

移动幂等域固定为 `(device_id, idempotency_key)`。同一域、同一规范化请求摘要的重试返回
`Duplicate` 并指向首次 command；同一域不同摘要返回 `IdempotencyConflict`。设备显示名是
transport 目录元数据，不得进入 Actor、授权判断或幂等域。内部 Broker/Workflow 仍可直接
构造受信 `CommandEnvelope`，但该入口不得暴露成 mobile business frame。

实现若把幂等域编码成存储键，必须对 actor kind、ID 与 key 使用具备长度边界的无歧义编码；
不得用裸分隔符拼接任意 UTF-8 字符串。请求摘要必须覆盖完整 `Command` 与 `ttl_ms`，不能只
比较 key。

本实现的 idempotency side table 使用带 `format_version = 2` 的 JSONL record，保存上述无歧义
键的 SHA-256 与 canonical `CommandId`。v0.2 的无版本、裸空格分隔记录必须在 store load 时
`MalformedRecord` fail-loud；不得因新旧键摘要不命中而把旧命令再次下发 runtime。T-107 在
加入请求摘要/outbox 时只能继续显式升版或事务迁移，不能启发式接受未知格式。

### 6.1 确认

```
CommandAck {
  command_id: CommandId,
  outcome: CommandOutcome,
  acked_at_ms: i64,
}

CommandOutcome =
  | AcceptedLocally   { note_ref: Option<ContentRef> }
  | AcceptedByRuntime { binding_handle: ProviderBindingHandle }
  | Enqueued          { entry_id: QueueEntryId }
  | Rejected          { error: CanonicalError }
  | Duplicate         { original_command_id: CommandId }
```

规范（R-P10）：

- `AcceptedLocally` 表示 Broker 已持久化，但 runtime 还没接受。UI 不得显示为
  「Agent 已收到」。
- `AcceptedByRuntime` 必须携带 Broker `ProviderBindingHandle`；上游原始确认 ID 仍只在
  adapter 私有绑定存储。
- `AcceptedByRuntime` 只能在 adapter 收到与本地 command 关联、足以证明 runtime 已接受的
  结构化响应或通知后产生。发送 bytes 成功或 transport API 无错误返回不足以产生该 outcome。它是
  `LiveControl` / `Controlling` 的唯一合格控制证据，见 §4.2 / §4.3。
- `AcceptedByRuntime` 必须是同一 `command_id` 已有 `AcceptedLocally` 后的后续事实；没有该
  前序事实或同一 command 的第二条 runtime acceptance，write path 必须拒绝且不得追加日志。
- `AcceptedLocally` 只能由 Broker 的 `submit_command` 路径写入；adapter 或通用 effect ingestion
  不得自行构造该 outcome。当前 `SubmitPrompt` 路径的 runtime ack 还必须关联唯一的
  `RemoteCommand` Turn，且 Turn 的 Session runtime 与 acknowledgement handle 一致。
- 相同 `(actor, idempotency_key)` 重复提交必须返回 `Duplicate` 并指向首次
  `command_id`，且不得对 runtime 重复下发。
- 命令过期（`expires_at_ms` 已过）必须以 `ErrorCode::CommandExpired` 拒绝。
- `CommandAck` **没有 cursor 字段**。若 ack 被写为
  `StateEffect::CommandAcknowledged`，包含它的 `LogRecord.cursor` 是唯一权威
  durable cursor，避免自引用「ack 的 cursor 指向包含自己的 record」。
- `AcceptedLocally.note_ref`、rework/skip reason 和 free-form attention 正文若存在，
  都必须是 Sensitive ContentRef，不得使用普通 String 旁路日志策略。

---

## 7. 错误

```
CanonicalError {
  code: ErrorCode,
  retriable: bool,
  detail_ref: Option<ContentRef>,
  at_ms: i64,
}

ErrorCode =
  | NotFound | InvalidCommand | CommandExpired | IdempotencyConflict
  | CapabilityUnsupported | CapabilityUnavailable
  | UpstreamBlocked { blocker_id: BlockerId }
  | ApprovalExpired | ApprovalAlreadyAnswered | JoinFailed
  | RuntimeUnavailable | RuntimeProtocolViolation
  | UpstreamRejected | UpstreamTimeout | AuthRequired
  | CursorGap | ContentEvicted | BackpressureDropped
  | Internal
```

规范：

- UI 只按闭合 `ErrorCode` 本地化安全摘要。协议不携带可入日志的自由文本 summary；
  上游错误原文与任何详情一律进入 Sensitive `detail_ref`。
- 人工拒绝审批**不是**错误（R-P8）。它表现为 `AttentionState::Answered` +
  `ItemStatus::Declined`。任何把 `Declined` 映射到 `ErrorCode` 的实现都是打回项。
- `CapabilityUnsupported`（runtime 根本不支持）与 `CapabilityUnavailable`
  （这条连接当前不可用）必须区分。
- `UpstreamBlocked { blocker_id }` 专用于「公开接口不存在」，`BlockerId` 是变体的
  强制字段，不是注释约定。

---

## 8. 投影

投影是订阅者读模型。每个投影都必须能由 §5 的快照与日志纯函数式导出，且带
`projection_version`，客户端据此判断是否需要全量刷新。

```
PROJECTION_VERSION = 2
```

| Projection | 内容 | 需求来源 |
|---|---|---|
| `ProjectIndexView` | provider 分组、跨 provider 全部项目、`SessionCounts`、`attention_count`、host `reachability` | REQUIREMENTS §2.1 |
| `SessionIndexView` | 活动/历史/归档分区、`SessionStatus`、`queue_depth`、`open_attention_count`、`ownership`、`live_binding` 摘要 | §2.1、§2.2 |
| `TranscriptView` | 按 `Turn` 分组的 `Item` 列表，含 `ContentRef` 与 `ItemStatus` | §2.2 |
| `LiveActivityView` | 当前 `Turn`、进行中 `Item` 的增量、`PlanUpdate`、`TaskUpdate` | OBJ-3 |
| `InputQueueView` | 有序 `QueueEntry`、`QueueState`、`editable` | §2.2、OBJ-6 |
| `AttentionInboxView` | 跨项目 `Open` 的 `AttentionItem`，按 `expires_at_ms` 排序 | §2.1、OBJ-6 |
| `WorkflowBoardView` | `Step` 依赖图、`StepState`、`Artifact` 交接、`human_gate` | §2.3、OBJ-7 |
| `RuntimeCapabilityView` | `CapabilityEntry` 列表，含 `state` 与 `evidence` 原因 | §4.2、OBJ-2 |

```
ProjectionKey =
  | ProjectIndex      { host_id: HostId }
  | SessionIndex      { project_id: ProjectId }
  | Transcript        { session_id: SessionId }
  | LiveActivity      { session_id: SessionId }
  | InputQueue        { session_id: SessionId }
  | AttentionInbox    { host_id: HostId }
  | WorkflowBoard     { workflow_id: WorkflowId }
  | RuntimeCapability { host_id: HostId, runtime_id: ProviderRuntimeId }

ProjectionEnvelope {
  projection_version: u32,
  key: ProjectionKey,
  cursor: Cursor,
  payload: ProjectionPayload,
}

ProjectionPayload =
  | ProjectIndex      { view: ProjectIndexView }
  | SessionIndex      { view: SessionIndexView }
  | Transcript        { view: TranscriptView }
  | LiveActivity      { view: LiveActivityView }
  | InputQueue        { view: InputQueueView }
  | AttentionInbox    { view: AttentionInboxView }
  | WorkflowBoard     { view: WorkflowBoardView }
  | RuntimeCapability { view: RuntimeCapabilityView }

ProjectIndexView {
  host_id: HostId,
  reachability: HostReachability,
  groups: Vec<ProviderGroup>,
}

ProviderGroup {
  family: ProviderFamily,
  runtime_ids: Vec<ProviderRuntimeId>,
  projects: Vec<ProjectSummary>,
}

ProjectSummary {
  project_id: ProjectId,
  display_name: String,
  bindings: Vec<ProjectBindingSummary>,
  session_counts: SessionCounts,
  attention_count: u32,
  workflow_count: u32,
  last_activity_at_ms: i64,
}

ProjectBindingSummary {
  binding_id: ProjectBindingId,
  runtime_id: ProviderRuntimeId,
}

SessionIndexView {
  project_id: ProjectId,
  active: Vec<SessionSummary>,
  history: Vec<SessionSummary>,
  archived: Vec<SessionSummary>,
}

SessionSummary {
  session_id: SessionId,
  project_binding_id: ProjectBindingId,
  title: Option<String>,
  status: SessionStatus,
  ownership: OwnershipMode,
  live_binding: LiveBinding,
  queue_depth: u32,
  open_attention_count: u32,
  last_activity_at_ms: i64,
}

TranscriptView {
  session_id: SessionId,
  turns: Vec<TranscriptTurn>,
  has_earlier: bool,
}

TranscriptTurn {
  turn: Turn,
  items: Vec<Item>,
}

LiveActivityView {
  session_id: SessionId,
  active_turn_id: Option<TurnId>,
  streaming_item_ids: Vec<ItemId>,
  plan: Vec<PlanEntry>,
  tasks: Vec<AgentTask>,
  updated_at_ms: i64,
}

InputQueueView {
  session_id: SessionId,
  entries: Vec<QueueEntry>,
  writable: bool,
  steer_supported: bool,
}

AttentionInboxView {
  entries: Vec<AttentionItem>,
}

WorkflowBoardView {
  workflow_id: WorkflowId,
  state: WorkflowState,
  steps: Vec<WorkflowBoardStep>,
  artifacts: Vec<Artifact>,
}

WorkflowBoardStep {
  step_id: StepId,
  title: String,
  state: StepState,
  depends_on: Vec<StepId>,
  assignment: StepAssignment,
  session_id: Option<SessionId>,
  blockers: Vec<StepBlocker>,
}

RuntimeCapabilityView {
  host_id: HostId,
  runtime_id: ProviderRuntimeId,
  negotiated_at_ms: i64,
  entries: Vec<CapabilityEntry>,
}
```

规范：

- 投影**不得**新增 canonical state 里没有的语义。它只能选择、排序、聚合和截断。
- 任何「不支持/未验证/被阻塞」都必须在投影中可见，禁止隐藏控件后宣布完成
  （REQUIREMENTS §1）。
- `ProjectionEnvelope.cursor` 只在同一个 `ProjectionKey` 内有意义。它由持久 projection
  journal 独立分配并严格 `+1`；canonical `StreamKey` 的 head 不得再冒充 projection cursor。
- 每条 journal entry 都是该 key 在该 cursor 下的**完整读模型**，不是增量 patch。canonical
  append 后按显式 fanout matrix 重算受影响 key；只有 payload 逐字段变化时才追加 entry。
- key 与 payload 必须逐一同名且 scope 一致：ProjectIndex 的 `view.host_id` 等于 key host；
  SessionIndex 的 `view.project_id` 等于 key project；Transcript、LiveActivity、InputQueue 的
  `view.session_id` 等于各自 key session；AttentionInbox 的每个 entry 都属于 key host；
  WorkflowBoard 的 `view.workflow_id` 等于 key workflow；RuntimeCapability 的
  `view.host_id` / `view.runtime_id` 分别等于 key host/runtime。
  任一错配必须 fail-closed。
- 同一 key 内 duplicate cursor、非 `previous + 1` 的 gap、cursor arithmetic overflow 都是
  journal/订阅错误；不得静默覆盖缓存、跳到 current 或复用其他 key 的 cursor。

---

## 9. 订阅、重放与背压

### 9.1 Canonical stream 重放（host 内部）

```
Subscribe { stream: StreamKey, since: Option<Cursor> }

SubscribeAck { stream: StreamKey, outcome: SubscribeOutcome }

SubscribeOutcome =
  | Resumed          { from_cursor: Cursor }
  | SnapshotFollows  { snapshot_cursor: Cursor }    // since 缺失或已被压缩时
  | Rejected         { error: CanonicalError }
```

快照可能很大，因此它**不是** `SubscribeAck` 的一个变体：控制响应只说明能否续传，
与所订阅 `StreamKey` 同型的 `SnapshotEnvelope` 作为独立消息在其后送达：Host、
Project、Session、Workflow 流分别发送 `HostSnapshot`、`ProjectSnapshot`、
`SessionSnapshot`、`WorkflowSnapshot`。

规范：

- `since` 指向的 cursor 已被压缩掉时，服务端必须返回 `SnapshotFollows` 并随后发送快照，
  不得静默从当前位置开始推送。
- 服务端**可以**合并同一对象的连续增量（背压），但必须记录
  `DiagnosticCode::BackpressureCoalesced`。
- 服务端**不得**丢弃状态转移。确实无法保序时，必须返回 `ErrorCode::CursorGap`，
  强制客户端重取快照。
- 推送唤醒只携带 `StreamKey` 与计数，不含任何 `ContentRef` 正文（§10、ADR-0011 D-3）。

`Subscribe` / `SubscribeAck` / `SnapshotEnvelope` / `LogRecord` 保留给 host 内部 canonical
恢复、诊断和复制。它们不是 R3 mobile transport 的业务 frame；Android 不接收 canonical
log/snapshot，也不实现 canonical reducer。

### 9.2 Mobile projection 订阅

```
ProjectionSubscribe {
  key: ProjectionKey,
  since: Option<Cursor>,
}

ProjectionSubscribeAck {
  key: ProjectionKey,
  outcome: ProjectionSubscribeOutcome,
}

ProjectionSubscribeOutcome =
  | Resumed        { from_cursor: Cursor }
  | CurrentFollows { current_cursor: Cursor }
  | Rejected       { error: CanonicalError }
```

`ProjectionSubscribe` 的 key 是授权、保留窗口与 cursor 的唯一域。服务端必须先在同一
锁/actor 顺序中注册 live tail，再读取该 key 的 floor/head 与 `since`，然后：

- `since.checked_next() >= floor` 时可连续恢复，返回
  `Resumed { from_cursor = since.checked_next() }`，随后从该 cursor 严格连续发送完整
  `ProjectionEnvelope`。因此 `since = floor - 1` 仍可恢复；`since = head` 时 from_cursor 是
  下一条尚未产生的 cursor，服务端不重发 head，只等待 live entry；
- 首次订阅（`since = None`），或 `since.checked_next() < floor` 时，返回
  `CurrentFollows { current_cursor = head }`，随后恰好发送该 cursor 的当前完整 projection；
- `since > head`、key 无权访问、key/payload 无法构造、版本不兼容或 cursor arithmetic
  overflow 时返回 `Rejected`。cursor 相关拒绝使用 `ErrorCode::CursorGap`，不得截断或回绕；
- 收到 ack 后只发送大于已发送 head 的 live entry。replay/current 与 live 的交界不得漏发或
  重发同一 cursor。

客户端对每个 `ProjectionKey` 分别持久化最后**完整验证并应用**的 cursor。任一 key 错配、
duplicate、gap、overflow 或 payload 验证失败都必须关闭该订阅，并用最后成功 cursor 重连；
不得静默接受 current。服务端 live channel 背压到无法继续保序时，返回/关闭为 CursorGap，
不能丢 entry 或让慢订阅者阻塞 Broker。Kotlin/Swift 只消费完整 projection callback。

---

## 10. 脱敏与保留

必须视为 `Sensitivity::Sensitive`，因此只能以 `ContentRef` 出现，且正文不得进入普通日志、
推送或 relay 元数据：

- 消息与推理正文；
- 工具调用参数与输出；
- diff 与文件片段；
- 完整文件系统路径（含项目根）；
- 上游原始 session/turn/item/request/ack ID（只允许进入 §3 的 adapter 私有绑定存储）；
- 任何 token、密钥、授权头。

日志中允许出现：canonical ID、枚举变体名、计数、`digest`、`byte_len`、时间戳、错误码。

保留策略：`ContentRef` 元数据随日志长期保留；正文可按大小与时间上限淘汰为
`ContentAvailability::Evicted`。已被淘汰的正文不得静默替换为空字符串。

---

## 11. Provider 映射附录：Codex app-server

本节的形状全部来自本仓库 2026-07-30 的真实录制，不是 schema 推断。
它是 T-100 的映射依据，也是三条 canonical 规范的一手证据来源。

### 11.1 映射表

| 上游 method | canonical 效果 |
|---|---|
| `thread/start` 响应 | adapter 私有存储 raw thread id，分配 `ProviderBindingHandle(kind = Session)`，再 `SessionUpserted`（`ownership = BrokerManaged`） |
| `thread/started` | 通过私有绑定表解析为 canonical session 后 `SessionUpserted`（幂等确认） |
| `turn/start` 响应 | adapter 私有存储 raw turn id，分配 `ProviderBindingHandle(kind = Turn)`，再 `TurnUpserted`（`status = Running`，`origin = RemoteCommand`） |
| `turn/started` | 通过私有绑定表解析为 canonical turn 后 `TurnUpserted`（幂等确认） |
| `thread/status/changed` `params.status.type == "active"` | `SessionStatusChanged { Running }` |
| `thread/status/changed` `params.status.type == "idle"` | `SessionStatusChanged { Idle }` |
| `item/started` | `ItemUpserted`（`status = InProgress`） |
| `item/agentMessage/delta` | `ItemUpserted`（追加 `AgentMessage.content`） |
| `item/completed` | `ItemUpserted`（按上游 item `status` 映射终态） |
| `item/fileChange/requestApproval` | `AttentionUpserted`（`Approval`，`join` 按 `params.itemId` 解析） |
| `item/commandExecution/requestApproval` | **不建模**，`DiagnosticRecorded { UnknownUpstreamMessage }`。见 [ADR-0014](adr/0014-codex-approval-families-and-timestamp-units.md) D-1：其回复的 `decision` 是与 file-change **不同构**的 `oneOf` |
| `item/permissions/requestApproval` | **不建模**，同上。其回复 `required` 是 `permissions`（`GrantedPermissionProfile`），**没有 `decision`**，套用 accept/decline 会给出 runtime 不接受的选项，违反 §4.7 |
| `turn/completed` | 从真实 `params.turn.status` 与 `params.turn.error` 归约为 `Completed` / `Failed` / `Cancelled`，**只更新状态、时间与失败时的 error** |
| 未登记 method | `DiagnosticRecorded { UnknownUpstreamMessage }` |

Item 类型映射：`userMessage → UserMessage`、`agentMessage → AgentMessage`（`phase`
取上游 `phase`，观测到 `commentary` 与 `final_answer` 两值）、`reasoning → Reasoning`、
`fileChange → FileEdit`、`commandExecution → ToolCall { surface: ShellCommand }`。
其余 item 类型不得进入已知 `ItemBody`；产生
`DiagnosticRecorded { code: UnknownUpstreamLabel }`，原始标签仅可放入 Sensitive
`detail_ref`。

上游 item `status` 映射：`inProgress → InProgress`、`completed → Completed`、
`declined → Declined`、`failed → Failed`。

**时间戳单位（[ADR-0014](adr/0014-codex-approval-families-and-timestamp-units.md) D-2）**：
同一批报文里两种单位并存，adapter 必须分别处理——

| 字段 | 上游单位 | canonical |
|---|---|---|
| `turn/started`、`turn/completed` 的 `params.turn.startedAt` / `completedAt` | **Unix 秒**（实测 `01-simple-turn.jsonl` 为 `1785378397`） | ×1000 转毫秒 |
| item 与审批的 `startedAtMs` / `completedAtMs` | 毫秒 | 原样 |

R-P2 要求 canonical 一律毫秒；漏掉这个换算会让 turn 的时间落到 1970 年附近而不报错。

真实 fixture 没有独立 `turn/failed` notification。失败必须只从
`turn/completed.params.turn.status/error` 归约；未知 status 产生
`DiagnosticRecorded` / `RuntimeProtocolViolation`，不得猜为相近的已知状态。

审批决定：canonical `DecisionSemantics::Allow → "accept"`、
`Deny → "decline"`（已实测两值均被 0.146.0 接受）。

### 11.2 三条一手证据

**证据 1 — 审批请求需要 join。**
`tests/fixtures/codex/03-permission-approve.jsonl:50` 的审批请求 params 只有
`threadId`、`turnId`、`itemId`、`startedAtMs`、`reason`、`grantRoot`，**没有**可展示的
diff 或命令内容。可展示上下文在 `:48` 的 item 报文里。因此 `JoinState` 是必填字段，
且必须处理「审批先到、item 后到」的顺序（→ `JoinFailureReason::ItemNotYetSeen`）。

**证据 2 — 拒绝不是错误。**
`tests/fixtures/codex/04-permission-deny.jsonl:50` 客户端回复 `{"decision":"decline"}`；
`:53` 该 item 以 `status: "declined"` 终结；`:84` 整个 turn 仍然是 `turn/completed`。
这证明 R-P8：`Declined` 是 Item 终态，Turn 未失败。

**证据 3 — 不要用「turn 结束」报文重建 transcript。**
`03-permission-approve.jsonl:83` 的 `turn/completed` 携带 `itemsView: "summary"`，
其 `items` 数组**只有最后一条 agentMessage**，而该 turn 实际包含 userMessage、
两条 reasoning、两条 agentMessage 和一条 fileChange。因此 `Turn.item_ids` 必须由逐条
item 转移累积，见 §4.4。

### 11.3 观测到但 v0.3 不使用的面

以下 method 在真实录制中出现，v0.3 **不**建模，一律走 `DiagnosticRecorded`：

`mcpServer/startupStatus/updated`、`thread/tokenUsage/updated`、
`account/rateLimits/updated`、`remoteControl/status/changed`。

其中 `remoteControl/status/changed`（录制值 `status: "disabled"`）与
`codex app-server daemon` 的控制 socket 是 R7 原生表面研究的线索，**不得**在 T-100 中
作为依赖使用（[ADR-0009](adr/0009-session-broker.md) D-5）。

---

## 12. v0.3 明确不含

| 项 | 归属 |
|---|---|
| 公网 rendezvous、relay 路由与 push | R4；R3 LAN 的 TLS、分帧、配对、认证与吊销由 [TRANSPORT 0.1](TRANSPORT.md) 定义 |
| Workflow 自动调度与自动质量评估 | ADR-0010 D-4 允许后做 |
| 文件树、代码预览、Git 命令投影 | R9 |
| OpenCode / Claude / ACP 映射附录 | R5，各自接入时追加 §11 同构章节 |
| 外部原生 CLI/GUI 附着的发现与绑定合同 | R7，当前为 `CapabilityState::UpstreamBlocked` |
| 多用户与团队权限 | REQUIREMENTS §7.3 明确不做 |

---

## 13. 与 `crates/kaleido-proto` 的对应

| 本文件章节 | proto 模块 |
|---|---|
| §3 | `ids` |
| §4.1 | `host` |
| §4.2 | `capability` |
| §4.3 | `session` |
| §4.4 / §4.5 | `turn` |
| §4.6 | `queue` |
| §4.7 | `attention` |
| §4.8 | `workflow` |
| §4.9 / §4.10 / §4.11 | `content` |
| §5 | `effect` |
| §6 | `command` |
| §7 | `error` |
| §8 | `projection` |
| §9.2 | `projection` |

`kaleido-proto` 是合同。修改它必须先修改本文件并走 ADR（`AGENTS.md` §2.1、
[MILESTONES](MILESTONES.md) 任务规则）。
