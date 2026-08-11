# ADR-0019: LiveControl 只由 runtime 接受命令证明

- 状态：**已接受，2026-08-09**
- 日期：2026-08-09
- 决策人：用户（项目主管）
- 任务：[T-104](../tasks/T-104.md)
- 触发：D-B1；`Capability::LiveControl` 与 `LiveBinding::Controlling` 当前结构性不可达
- R5 修订：UACP `0.5.0` 为 runtime ack 增加 mandatory Session scope 与
  `RuntimeAcceptanceKind`；不改变“只有结构化 runtime acceptance 才能证明控制”的原决策

## 背景

当前 Codex adapter 能从实时结构化流量证明 `LiveObserve`、`TurnPrompt` 与
`InteractionApproval`，但没有任何路径证明 `LiveControl`。因此即使 Broker 已经提交
prompt、回答审批并继续收到 runtime 状态，Session 仍只能是 `Observing`。

现有合同还没有精确定义 `LiveControl` 是具体动作能力，还是多个动作能力之上的会话级
事实。若直接在 bootstrap、版本判断或发送成功时把它设成 Supported，会把「尝试控制」
伪装成「runtime 已接受控制」；若把它解释成只能控制外部创建的会话，则
`BrokerManaged` 的手机干预又没有可靠的通用状态可读。

## 决策

### D-1 采用上位概括语义

`LiveControl` 表示：在当前 runtime connection 上，至少一个针对某个 live Session、会
改变 runtime 状态的 Broker 命令，已经被 runtime 接受。

它是 `TurnPrompt`、`InteractionApproval`、`TurnInterrupt` 等具体能力之上的**已取得控制
证据**，不替代这些具体能力：

- `LiveControl::Supported` 不推出 `TurnSteer::Supported`；
- `Controlling` 不表示所有控制按钮都可用；
- 一个具体动作仍必须读取自己的 capability，不能只读取 `LiveControl`。

### D-2 唯一合格证据是 `AcceptedByRuntime`

只有与本地 `CommandEnvelope.command_id` 相关联且 scope 完整的
`CommandOutcome::AcceptedByRuntime { session_id, acceptance_kind, binding_handle }`
可以证明 `LiveControl`。

以下均不合格：

- `AcceptedLocally` 或 `Enqueued`；
- 子进程写入成功、JSON-RPC bytes 已发送或无错误返回；
- provider 名称、版本范围或 launch surface；
- handshake 声明但未发生任何真实控制；
- fixture replay 中录下来的历史接受；
- 观察到一个无法关联到本地命令的外部状态变化。

具体 adapter 必须先收到能够证明 runtime 接受的结构化响应或通知，再构造
`AcceptedByRuntime`。该 outcome 的 canonical `SessionId` 与 `RuntimeAcknowledgement`
binding handle 由 Broker 解析/铸造；上游原始 ID 不进入 canonical state。write path 必须验证
Session 与 handle 指向同一 runtime。

Codex 当前纵切使用 `turn/start` 的结构化 response 作为 `SubmitPrompt` 被 runtime 接受的
证据。仅发送 request 不够；response 必须成功解码出钉定的 turn ID 与 status。未来其他
控制命令必须分别找到同等级的 public structured acknowledgement，不得复用本 ADR 作为
乐观提升许可。

canonical state 还必须校验这条证据的历史关联：`AcceptedByRuntime` 之前必须已经存在同一
`command_id` 的 `AcceptedLocally`，同一命令的第二条 runtime acceptance 必须拒绝。这样即使
adapter 出错，也不能把一条无本地来源或重复的 runtime ack 写进 durable log。

`RuntimeAcceptanceKind::PromptTurn` 必须把该命令关联到唯一的
`TurnOrigin::RemoteCommand { command_id }`：Turn 的 Session 所属 runtime 必须与
`RuntimeAcknowledgement` handle 一致。`Controlling` write path 反向检查这条
Session → Turn → command → runtime ack 链；只有 capability、另一个 Session 的 ack、跨 runtime
handle，或经通用 effect ingestion 伪造的 `AcceptedLocally` 都不够。

`RuntimeAcceptanceKind::SessionControl` 用于 interrupt 等不创建 Turn 的结构化控制 receipt；
它仍必须携带目标 canonical Session、同 runtime acknowledgement handle 与本地 command
correlation，但不得伪造一个 RemoteCommand Turn 来通过 `PromptTurn` 守卫。两种 kind 不能互换。

该关联一旦写入即不可被后续 effect 改写：同一 remote command 不能创建第二个 Turn，既有
Turn 不能更换 Session、origin 或 provider binding identity。live Session 的更新也必须使用候选
对象自身可解析的 runtime 引用，不能借 store 中旧 Session 的绑定通过验证。

### D-3 `Controlling` 是 Session 级、连接存活期间锁存的证据

某 Session 的本地命令第一次得到 `AcceptedByRuntime` 后：

1. 该 runtime connection 的 `LiveControl` 变为 Supported，证据来源为
   `ObservedInTraffic`；
2. 只有该 Session 的 live binding 从 `Observing` 变为 `Controlling`；
3. `since_at_ms` 保留 live binding 最初建立的时刻，`evidence.observed_at_ms` 记录首次控制
   被接受的时刻；
4. 后续命令可以各自产生 runtime ack，但不需要重复改变 binding；
5. runtime 断开后，现有规则仍把 binding 置为 `NotBound`，不能因历史控制继续显示在线。

同一个 runtime 上的另一 Session 不得仅因 connection 级 `LiveControl` 已 Supported 就自动
成为 `Controlling`；它必须取得自己的 Session 级接受证据。

三种 `OwnershipMode` 使用同一门槛：

| ownership | `Controlling` 的含义 |
|---|---|
| `BrokerManaged` | Broker 创建的 Session 已至少成功执行一个 runtime 命令 |
| `SharedRuntime` | Broker 在共享 Session 上已至少成功执行一个 runtime 命令 |
| `ExternalNative` | Broker 附着后已至少成功执行一个 runtime 命令 |

ownership 不构成能力证据，也不放宽门槛。

### D-4 replay 与无关联流量不能提升

Replay 只证明录制报文可以解码。它没有当前 live connection，也没有本轮进入 Broker 的
`CommandEnvelope`，因此：

- 不产生 `AcceptedByRuntime`；
- 不证明 `LiveControl`；
- `LiveBinding` 继续为 `NotBound`；
- 录制中的 `turn/start` response 不能反推成本地控制。

实时流量若没有具体本地 command correlation，也同样不能生成 runtime ack 或提升控制。

### D-5 R3 手机按钮读取 live 状态与具体能力

手机显示或启用某个干预按钮时必须同时满足：

1. Session 的 `live_binding.is_live()` 为真；
2. 该动作对应的具体 capability 是 `Supported`。

例如 steer 按钮读取 `TurnSteer`，审批按钮读取 `InteractionApproval`。`Controlling` 用于展示
「本 Session 已取得过 runtime 控制证据」以及审计状态，不是所有按钮的总开关。
因此 T-104 证明 `LiveControl` 时不得顺带提升 `TurnSteer`。

## 实现边界

本卡只为 Codex `SubmitPrompt` 建立第一条可达路径：

- provider-neutral `submit_prompt` 接口携带真实 `CommandId`；
- Codex reducer 只为明确关联的 `turn/start` response 产生
  `AcceptedByRuntime`；
- 该 response 产生的 Turn 使用本次关联命令的 `RemoteCommand` origin，而不是由 connection
  模式猜测来源；
- capability 更新必须先于 `Controlling` Session upsert 应用，保持 write-path 校验；
- hostd 记录本地 ack 与 runtime ack，并在诊断报告中锁存 `Controlling` 瞬间；
- state write path 拒绝无前序本地接受或重复的 runtime ack；
- 不修改 proto wire shape、schema、fixture、queue/steer 状态机或移动端代码。

R5 后续在 UACP `0.5.0` 扩展了该既有语义：`AcceptedByRuntime` wire shape 增加 mandatory
`session_id` / `acceptance_kind`；Codex 原 `SubmitPrompt` 使用 `PromptTurn`，OpenCode/Claude
prompt 同样使用 `PromptTurn`，两家的 structured interrupt 使用 `SessionControl`。这是对跨
Session 防伪造守卫的闭合，不授权任何未获 runtime receipt 的乐观提升。

## 拒绝的方案

### A. Session 一建立就证明控制

拒绝。建立连接或创建 thread 只证明观察/会话创建，不证明任一会话命令被 runtime 接受。

### B. `TurnPrompt::Supported` 自动推出 `LiveControl`

拒绝。能力声明或过去流量不能替代本 Session 的命令接受事实。

### C. 写入子进程 stdin 成功就生成 runtime ack

拒绝。这只能证明本地 transport 接受 bytes，不能证明 runtime 解码并接受命令。

### D. `Controlling` 自动推出全部具体控制能力

拒绝。会错误提升 `TurnSteer` 等尚未验证的动作。

## 后果

- `LiveControl` 与 `Controlling` 在真实 Codex prompt 接受后可达；
- 读模型可以区分只观察、已取得控制证据和已离线；
- canonical log 会为同一 command 保留先前的 local ack 与后续 runtime ack，这是 R-P10
  两个不同事实；
- replay、仅观察、发送失败和无 runtime response 的路径继续 fail closed；
- 未来每个新控制动作仍需自己的结构化接受证据，不能借本 ADR 放宽判断。
