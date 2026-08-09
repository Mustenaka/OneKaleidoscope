# ADR-0018: Attention 应答来源区分本地命令与外部观察

- 状态：**已接受，2026-08-09**
- 日期：2026-08-02
- 决策人：项目主管
- 任务：[T-103](../tasks/T-103.md)
- 触发：P-1；`AttentionState::Answered.command_id` 在 replay 中指不到任何本地命令
- 授权边界：`AttentionState` 及其直接相关类型；本轮由用户以主管身份明确批准，协议与代码同步落地

## 背景

当前 `AttentionState::Answered` 强制携带 `command_id`。本地
`RespondAttention` 命令确实有真实 `CommandEnvelope`，该引用成立；但 Codex transcript
也会报告一个 Broker 没有发出的审批决定。T-100 replay 走的就是后一条路径，当前 reducer
只能铸造一个确定性 `CommandId` 填入字段。这个 ID 代表的是「被观察到的决定」，并不引用
任何 `CommandEnvelope`，因此会让移动端按 ID 回查时得到空洞。

同一问题会出现在未来的 `shared_runtime`：原生 CLI/GUI 可以在 Broker 正观察的 live
runtime 上作出决定。因而 `ObservedInTraffic` 只说明报文来自实时流量，**不能**说明决定由
Broker 的本地命令触发。当前 reducer 按整条连接的 `EvidenceSource` 抑制 live reply，无法区分
「本地命令的 wire 回显」和「同一 runtime 上的外部客户端应答」。

## 决策

### D-1 `Answered` 使用闭合的 `AttentionAnswerSource`

将 §4.7 的形状改为：

```text
AttentionState =
  | Open
  | Answered {
      option_id: Option<String>,
      free_form_ref: Option<ContentRef>,
      decided_at_ms: i64,
      answer_source: AttentionAnswerSource,
    }
  | Expired    { at_ms: i64 }
  | Superseded { by: AttentionId }
  | Cancelled  { at_ms: i64 }

AttentionAnswerSource =
  | LocalCommand {
      command_id: CommandId,
    }
  | ObservedExternal {
      evidence: AttentionAnswerEvidence,
    }

AttentionAnswerEvidence {
  observer_host_id: HostId,
  observed_at_ms: i64,
  source: AttentionAnswerEvidenceSource,
}

AttentionAnswerEvidenceSource =
  | ObservedInTraffic
  | RecordedFixture
```

`LocalCommand.command_id` 必须引用实际进入 Broker 的 `CommandEnvelope`；不得铸造一个 ID
代表上游决定。`ObservedExternal` 根本没有 `command_id` 字段，使「外部观察却伪装成本地命令」
成为类型上不可表达的状态。

选择嵌套 `answer_source`，而不是拆成 `AnsweredByCommand` / `AnsweredExternally` 两个
`AttentionState` 变体，是为了不复制 option、free-form 和决定时间这组共同字段。该形状仍只含
带命名字段的 enum/record，符合 R-P1 和已经由 T-102 验证的 UniFFI 表达面。

### D-2 外部观察证据只陈述能够证明的事实

`AttentionAnswerEvidence` 的三个字段语义如下：

- `observer_host_id`：实际运行 Broker/adapter 并观察到该决定的 Host。它必须非空，且必须
  等于外层 `AttentionItem.host_id`；显式重复是为了让 evidence 自身可审计，validator 防止漂移。
- `observed_at_ms`：Broker 在 live 流量中收到该决定的时刻，或 recorder 原始记录中的观察时刻。
  replay 必须使用记录时刻，不得使用本次 replay 的墙钟，否则同一输入不能确定性收敛。
- `source`：证据媒介只能是实时结构化流量或真实录制 fixture。

`decided_at_ms` 仍表示决定时刻。若上游没有独立的决定时间，则使用同一结构化报文的观察时刻；
不得猜测更早的时间。

该证据**不声明是谁作出了决定**。现有上游报文没有经过认证的外部 actor 身份；把它写成
`Actor::Human`、设备标签或 `Actor::Broker` 都是在猜。`observer_host_id` 表示「谁观察到」，
不是「谁决定」。

### D-3 不复用 `CapabilityEvidence` 或其 `EvidenceSource`

不复用，理由有四点：

1. `CapabilityEvidence` 是 runtime connection 级的能力证明，外部应答证据是单个
   `AttentionItem` 的事实，两者作用域不同。
2. `EvidenceSource` 还允许 `HandshakeDeclared`、`ManualAcceptance` 与 `Absent`；三者都不能
   证明一项具体应答已经发生。若复用，必须靠 validator 禁掉大半个 enum，说明类型边界错误。
3. `ObservedInTraffic` 本身不能区分本地命令回显与外部客户端应答；来源判断还必须依赖
   本地命令/发送关联。
4. `CapabilityEvidence.note_ref` 对应能力说明，不应被借来存放应答或上游原文。

因此新增作用域更窄的 `AttentionAnswerEvidenceSource`。虽然两个合法变体与现有 enum 同名，
它们是不同合同，不能互换。

### D-4 来源由本地命令关联决定，不由连接模式决定

构造规则是：

| 场景 | canonical 来源 | 状态效果 |
|---|---|---|
| Broker 接受本地 `RespondAttention` | `LocalCommand { command_id }` | 使用真实 envelope ID 更新为 Answered |
| 随后看到与该发送关联匹配的 wire reply | 仍为原 `LocalCommand` | 只验证/确认，不得用第二个 Answered 覆盖 |
| live runtime 出现无法关联到本地命令的 reply | `ObservedExternal { ObservedInTraffic }` | 产生外部观察的 Answered |
| replay 真实录制中的 reply | `ObservedExternal { RecordedFixture }` | 产生外部观察的 Answered |

「无法关联」不是超时猜测，而是 Broker 没有该 attention/request 的本地
`CommandEnvelope`/发送关联。仅凭 `ReducerConfig.evidence == ObservedInTraffic` 不得判成本地。

因此阶段二应删除当前按整条连接维护的 `suppressed_attention_upserts` 语义，改成针对具体
attention/request 的本地发送关联。实现可以抑制已关联 wire 回显的重复 state effect，但
抑制条件必须是**本地命令关联**，不能是 live/replay 模式。后续 join refresh 也不得把已经确定的
`answer_source` 改写成另一来源。

### D-5 `check_reply` 对两种 Answered 使用同一终态错误

无论来源是哪一种，`Answered` 都是不可再次回答的终态。`AttentionItem::check_reply` 对两者均
返回 `ReplyRejection::NotOpen`，store 映射为现有
`ErrorCode::ApprovalAlreadyAnswered`。

不新增错误码：`ApprovalAlreadyAnswered` 准确描述当前事实，不声称首次应答来自本地。
也不能返回 `CommandOutcome::Duplicate`：外部观察没有原始本地 `(actor, idempotency_key)` 或
`command_id` 可供 `Duplicate.original_command_id` 引用。只有重复提交同一个本地幂等键时才返回
`Duplicate`；在外部应答后新发一个本地回复命令，应得到
`Rejected { error.code = ApprovalAlreadyAnswered }`，且不得下发 runtime。

### D-6 这是 `0.2.0` 兼容边界；旧 durable log 不猜测迁移

替换 `Answered.command_id` 并新增闭合 enum 会改变 wire 形状。PROTOCOL §1 明确规定：

- `0.1.x` patch 只允许不改变 wire 形状的修复；
- 任何破坏性变更必须提升 pre-1.0 minor。

所以若本 ADR 获批，阶段二必须把 `PROTOCOL_VERSION` 从 `0.1.0` 提升为 `0.2.0`，同步更新
§1、`kaleido-proto::PROTOCOL_VERSION` 与兼容性测试。旧 `0.1.x` peer 必须在业务消息前拒绝，
不得把新 enum 当作 patch 兼容。任务卡虽只点名 §4.7/`attention.rs`，这个最小版本改动是满足
现有正式合同的必要后果，需由主管随本 ADR 明确批准；若不批准，就不存在合规的阶段二实现。

现有 durable log 的 `LogRecord` 没有内嵌 schema/version，旧 Answered 行只有
`command_id`，无法可靠知道它是真实本地命令还是 T-100 replay 铸造的占位 ID。禁止以下迁移：

- 把所有旧 `command_id` 当成本地命令；
- 按 ID 前缀、重新运行 mint 或 provider/launch surface 猜来源；
- 仅因别的 stream 中找不到 `CommandAcknowledged` 就断言是外部应答（日志可能被单流导出）。

本卡选择**不兼容、fail-loud**：新类型不为旧 `command_id` 提供 serde 默认值或兼容别名。
当前 `StreamLog::read_all` 会先解码全部记录，再向 canonical state 应用任何 effect，因此遇到旧
Answered 行必须以 `MalformedRecord` 终止加载，不会产生半重建状态。

升级处置：旧 store root 先归档为只读证据，再从真实 fixture、runtime history 或新的 live
流量重建到一个新的 `0.2` store root。T-100 已产出的日志继续作为门禁证据保留，但不得由新
binary 原地续写。若未来存在不能重建的用户日志，需另开迁移卡并定义带来源证据的显式迁移输入；
T-103 不写启发式 migrator，也不修改 `LogRecord`。

## 拒绝的方案

### A. `command_id: Option<CommandId>`

拒绝。`None` 只表示缺失，不能说明为何缺失、谁观察到、何时观察或证据来源；同时允许本地命令
路径错误地遗漏 ID，继续制造不可审计状态。

### B. 保留必填 `command_id` 并约定某个前缀代表外部

拒绝。它继续留下指不到命令的引用，把语义藏进字符串约定，并重复当前 P-1。

### C. 复用 `CapabilityEvidence`

拒绝。其作用域和合法来源集合都过宽，见 D-3。

### D. 自动推断旧日志里的来源

拒绝。旧记录缺少可靠、同记录内的来源证据；跨 stream 相关性不能把缺失证明成外部事实。

## 后果与阶段二边界

获批后，阶段二应且仅应：

1. 先更新 PROTOCOL §1、§4.7，再更新 `kaleido-proto` 的版本常量、
   `AttentionState` 与上述直接相关类型；
2. 更新 `check_reply`/store 契约测试，证明本地与外部两种来源都不可重复回答，且外部路径返回
   `ApprovalAlreadyAnswered`；
3. 更新 Codex reducer：删除合成 `CommandId`，按 D-4 的具体本地关联构造来源；
4. 用三份真实 fixture 保持 T-100 replay 语义，并用 slice run 证明本地审批仍携带真实命令 ID；
5. 增加旧 `0.1` Answered 日志 fail-loud 的错误路径测试，不修改旧日志本体。

本 ADR 不处理 D-B1、D-B2、D-B6、D-B7、D-B8、D-B11，不修改能力、session、workflow、
transport、移动端、schema 或 provider fixture，也不把任何已登记项写成已解决。

## 已批准的三个点

1. `AttentionAnswerSource` / `AttentionAnswerEvidence` 的精确形状与字段名；
2. 不复用 `CapabilityEvidence`，外部重复回答继续使用 `ApprovalAlreadyAnswered`；
3. 按 PROTOCOL §1 将兼容边界提升为 `0.2.0`，并对旧 durable log 采用归档重建、fail-loud、
   不做启发式迁移。
