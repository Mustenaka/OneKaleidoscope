# ADR-0026: OpenCode 采用生成 REST/SSE adapter 与 snapshot-first 恢复

- 状态：**已接受，2026-08-10**
- 范围：R5 OpenCode Broker 管理/共享 server 会话
- 相关：[ADR-0005](0005-schema-normalization-layer.md)、[ADR-0008](0008-version-compatibility-model.md)、[ADR-0009](0009-session-broker.md)、[T-111](../tasks/T-111.md)
- 结清：D-B11 的实现路径；最终门禁见 [T-111 evidence](../gates/T-111-evidence.md)

## 背景

OpenCode 提供公开 HTTP server：REST 可以列出、读取和创建 session，SSE 可以观察实时
session/message/part/permission/question 事件。它不需要也不允许通过 PTY、TUI 或 transcript
轮询取得实时状态。

R5 开卡时仓库快照和支持基线仍是 `1.18.8`，本机真实 fixture 已来自 `1.18.11`，npm
current 为 `1.18.16`。同时，OpenCode 的 `/doc` 是 OpenAPI 3.1，不能直接交给只理解旧
JSON Schema 表达的生成器。手写一套相似 Rust DTO 会绕过漂移守卫，违反 ADR-0005 与
`AGENTS.md`。

另一个合同边界是 `/event` SSE 没有可恢复 cursor。断线后即使重新读取 REST 历史并打开新
SSE，也不能证明 snapshot 与新 tail 之间绝对没有事件窗口。

## 决策

### D-1 原样 OpenAPI 快照是唯一上游类型来源

`schemas/opencode/openapi.json` 保存真实 `1.18.16` `/doc` 原样响应。adapter 的类型链固定为：

```text
原样 OpenAPI 3.1 快照
  -> 纯机械规范化
  -> 从 PROTOCOL 所需 operation/type 提取闭包
  -> 构建期生成 Rust 类型
  -> adapter 私有 reducer
```

规范化产物与生成物只写构建目录，不提交，也不穿透到 canonical、UniFFI 或移动端。
当前 `1.18.16` 全文不需要规范化改写。审计删除了旧的
`numeric_exclusive_minimum_to_bound`：数值 `exclusiveMinimum` 本来就是 OpenAPI 3.1 / JSON
Schema draft-07 的合法严格下界，把它改写成 boolean 形式反而会降回旧 draft 语义。生成链直接保留
原约束，并移除了不能正确理解 3.1 schema 的 OpenAPI 3.0 parser。构建报告必须打印生成闭包规模；
当前为 125 schemas。零命中规则不得保留，规则超过 10 条必须重新评估生成器。

### D-2 REST 证明 history，SSE 证明 live

- `GET /session`、`GET /session/{id}` 与 `GET /session/{id}/message` 只证明
  `HistoryList` / `HistoryRead` / `HistoryResume` 和 REST snapshot；
- 只有当前连接真实读到并成功解码 `/event` SSE 后，才证明 `LiveObserve`，Session 才能从
  `NotBound` 进入 `Observing`；
- 一个同时与本地 `CommandId`、目标 canonical `SessionId` 关联的结构化 runtime receipt 才能产生
  `AcceptedByRuntime`，进而按 ADR-0019 提升 `LiveControl` 与该 Session 的
  `Controlling`；
- endpoint 存在、版本号、HTTP 请求发送成功或 fixture replay 都不能单独证明能力。

OpenCode 的 raw project/session/message/part/request/event ID 只存 adapter 私有绑定表。
project scope 以 server 返回的规范化 directory 与已选项目根一致为边界；opaque
`projectID` 不能替代目录校验。

当前 composition 连接用户明确提供的 OpenCode shared server，所以 REST 发现和 `POST /session`
创建的 Session 都标为 `SharedRuntime`；创建一条 Session 不等于 hostd 拥有 server 进程生命周期。
未来只有在 hostd 真实启动并管理整个 server 进程时才能使用 `BrokerManaged`。

### D-3 prompt、queue、interrupt 与 Attention 各自需要结构化接受

- prompt 优先走 `/api/session/{id}/prompt` 的 v2 admission receipt。receipt 与本地命令、目标
  canonical Session 和唯一 RemoteCommand Turn 相关联，并标为 `PromptTurn` 后才证明
  `TurnPrompt`；delivery 为 queue 时才证明 `QueueWrite`；
- 老 server 的 `prompt_async` 可以作为兼容发送路径，但 204/无 receipt 不产生 runtime
  acknowledgement，也不提升 prompt/queue 能力；
- interrupt 只有 `/session/{id}/abort` 对当前 active turn 成功后才证明
  `TurnInterrupt`，并产生携带目标 canonical `SessionId`、标为 `SessionControl` 的 runtime
  acknowledgement；不得为 interrupt 伪造 Turn；
- permission/question 回复只有对应公开 endpoint 成功后才完成 adapter 命令。QuestionSet
  按题保持 key、单/多选和 free-form，不压扁为一个答案；
- `QueueWrite` 不推出 `TurnSteer`。没有当前 turn 的结构化注入确认时，steer 意图继续
  `Pending`。

### D-4 无 cursor 的恢复必须显式标为非无损

SSE 断线后的恢复顺序固定为：

1. 读取目标 Session 与 messages 的新 REST snapshot；
2. reducer 以 payload event id 去重，并以 provider raw entity identity/canonical binding 收敛；
3. 打开一条新的 SSE tail；
4. 返回 `lossless_replay = false`。

重复 event id 不产生第二次 effect，重复 snapshot 不得制造第二个 canonical 实体；scope/identity 冲突必须
fail-closed。但因为 OpenCode 没有 SSE cursor，系统不得宣称 snapshot 与新 tail 之间
无 gap、不得把该路径写成 cursor resume。UACP projection journal 的移动端 cursor 仍可
精确恢复；这是 Broker 输出恢复，不是 provider 输入流的无损回放。

### D-5 D-B11 的版本结论

`1.18.8`、`1.18.11` 与 `1.18.16` 分别保留既有 T-004 证据、真实 simple-turn fixture、
真实 question fixture/本地 live probe。仓库快照升级为 `1.18.16`，required surface
按本纵切实际读取的 REST/SSE/v2 admission/question 类型扩展。warning candidate 收紧为
`=1.18.16`；早期 fixture 只作历史兼容回归，不把缺少逐版本 schema/runtime 证据的区间
冒充连续支持。

最新 exact `1.18.16` probe 又证明真实 `/event` 的 prompt-admitted timestamp 为 string，而同版本
`/doc` 生成类型要求 number；流中另有未声明的 `server.heartbeat`。adapter 按本 ADR fail-closed，
不写手工 DTO 或宽松 JSON 绕过。因此该 exact candidate 当前不构成 realtime support acceptance，
D-B11 仍未结清。最终必须先解决/审查上游合同漂移，再通过 `schema diff`、fixture 校验、live probe
和 CI；范围外版本继续允许检查并显示 unverified warning。

## 被否决的方案

| 方案 | 原因 |
|---|---|
| 手写 OpenCode DTO | 会形成静默漂移的影子协议，违反上游类型纪律 |
| 修改 `schemas/` 快照以迎合生成器 | 破坏原样漂移基准 |
| REST message 轮询冒充实时 | 不能保序表达 delta、等待人工和工具生命周期 |
| 将 SSE event `id` 当 replay cursor | `/event` 没有公开的 cursor resume 合同，观察到 ID 不等于可续订位置 |
| reconnect 后声明无损 | snapshot 与新 tail 之间仍存在不可证明的窗口 |
| endpoint/版本推导能力 | 绕过真实 runtime acceptance，违反 ADR-0008/0019 |

## 后果

- OpenCode 与 Codex 复用同一 canonical state、projection、command 与移动 UI；
- 新的上游 shape 必须先进入快照/required surface/生成链，再进入 reducer；
- OpenCode 可真实发现历史；实时观察/控制只有在生成合同与真实 stream 一致且 live gate 通过后
  才能声明，当前 `1.18.16` 漂移仍阻塞该声明；
- provider 断线可收敛恢复，但产品必须持续展示其输入 replay 不是无损的；
- 外部原生 TUI/GUI 的第三方附着仍归 R7，本 ADR 不把 shared server 等同于任意原生进程附着。
