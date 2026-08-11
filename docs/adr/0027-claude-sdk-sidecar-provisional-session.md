# ADR-0027: Claude 使用官方 Agent SDK sidecar 与 provisional Broker Session

- 状态：**已接受，2026-08-10**
- 范围：R5 Claude Agent SDK managed session
- 相关：[ADR-0009](0009-session-broker.md)、[ADR-0019](0019-live-control-runtime-acceptance.md)、[ADR-0025](0025-question-set.md)、[T-112](../tasks/T-112.md)
- 钉定：`@anthropic-ai/claude-agent-sdk@0.3.226`

## 背景

Claude Agent SDK 的公开 API 是 TypeScript：`query()` 提供流式消息、resume、interrupt 与
`canUseTool`；`listSessions()` 提供会话发现。Rust 若重写 SDK message/tool 类型，会产生一套
无法由官方类型检查守卫的上游 DTO。读取 `~/.claude` transcript、PTY/TUI 抓屏或逆向
Remote Control 同样不属于公开实时合同。

SDK 还有一个启动时序差异：`query()` 可以先准备好输入流，但 Claude raw session id 通常在
第一条 SDK init/message 后才出现。`kaleido-hostd` 的 runtime registry 则需要在 provider
启动时就有稳定 canonical Session 作为路由目标。如果等待第一条 prompt 才创建 Session，
多 runtime host 无法完成一致 bootstrap；如果提前伪造 raw Claude id，又会污染绑定与恢复。

## 决策

### D-1 官方 SDK 类型只在 TypeScript 边界消费

仓库内钉定 `@anthropic-ai/claude-agent-sdk@0.3.226`、lockfile 与 TypeScript 编译器。
sidecar 直接导入官方 SDK 类型与公开 API，包括官方 `AskUserQuestionInput`；CI 运行严格
typecheck。

sidecar 只输出 OneKaleidoscope 自有的闭合、版本化 frame：

```text
official SDK typed message/callback
  -> one-kaleidoscope sidecar frame v1
  -> Rust closed frame decoder
  -> canonical StateEffect
```

这些 frame 是本项目的进程桥合同，不冒充 Claude 上游类型。未知 kind 进入诊断；畸形 frame、
错误 protocol/version、缺 scope 或不匹配的 ID fail-closed。raw Claude
session/message/tool/request ID 只进入 adapter 私有绑定/fixture，不进入 canonical state。

### D-2 `ready` 创建 provisional Broker-managed Session

sidecar 在成功构造 `query()` 后发出带 `cwd` 的 `ready`。adapter 此时铸造稳定 canonical
Session，语义固定为：

- `ownership = BrokerManaged`；
- `history_source = None`；
- `live_binding = NotBound(NeverStarted)`；
- `binding_handle = None`；
- Session 与 Project 的 canonical ID 来自 Broker mint，不使用伪造 Claude raw ID。

这只是“Broker 已拥有一个可路由的待启动会话槽位”，不证明 Claude 已建立上游 Session、
不证明 history/live/capability，也不能展示为已连接原生 Claude UI。

收到第一条带真实 raw session id 的 `session_started` / `session_resumed` 后，adapter 复用同一
canonical Session ID，创建私有 provider binding，并分别根据真实流量更新
`HistorySource::ProviderApi`、`LiveBinding` 与能力。这样 hostd 路由身份在第一条 prompt 前后
不改变，也不会把 provisional identity 冒充上游 identity。

旧真实 fixture 的 `ready` 不含 `cwd`；它仍可作为历史失败证据解码，但不创建 provisional
Session。当前 live sidecar 必须带 `cwd`。

### D-3 discovery 与 resume 使用 SDK 公共 API

`listSessions({ dir })` 产生 Project 与离线 Session metadata 投影，只证明 `HistoryList`。
`HistoryRead` 必须由官方 `getSessionMessages()` 的真实消息结果另行证明；仅有 list summary
时保持 `NotVerified`。二者都不能证明 live attach。resume 只把 SDK 接受的 raw session id 作为 adapter
私有输入传给 `query({ resume })`；只有真实 `session_resumed` 才证明 `HistoryResume`。

由 `listSessions()` 发现的 Session 使用 `ownership = ProviderManaged`：这只表达公开 provider SDK
管理持久化并允许 structured list/resume，不表达 Broker 拥有该会话，也不表达原生表面拥有它。
provisional route slot 在得到真实 provider identity 前仍是 `BrokerManaged`；真实 discovered Session
则不得标成 `ExternalNative`。

SDK managed session 与独立 Claude CLI/GUI 是不同 ownership/binding 事实。当前没有公开合同
允许第三方附着任意原生 CLI/GUI；后者保持 `UpstreamBlocked` 并归 R7。

### D-4 runtime acknowledgement 必须等 SDK 消费或返回

- sidecar 的输入队列 `push` 只有在官方 `query()` 消费该 SDK user message 后才完成；随后
  `prompt_accepted` 才能与本地 `CommandId`、目标 canonical `SessionId` 和唯一
  RemoteCommand Turn 关联，产生 `AcceptedByRuntime { acceptance_kind = PromptTurn }`；
- `interrupt` 必须等待 `activeQuery.interrupt()` 的结构化结果，产生
  `AcceptedByRuntime { acceptance_kind = SessionControl }`，不得为 interrupt 伪造 Turn；
- permission/question 必须在官方 `canUseTool` callback 中创建请求，等待本地回答，再返回
  SDK `PermissionResult`；
- `AskUserQuestion` 使用官方 typed input，逐题映射为 ADR-0025 QuestionSet，答案再逐题映回
  SDK `updatedInput`；
- 观察到外部结果而没有本地 command association 时只能记录
  `ObservedExternal`，不得伪造 `LocalCommand`。

写入本地队列、子进程 stdin 成功、sidecar `ready`、SDK init capabilities 或 fixture replay
都不单独提升具体 capability。queue 没有结构化 steer receipt 时继续保持 `Pending`。

### D-5 真实证据边界

当前已提交 fixture 来自真实 `0.3.226` SDK managed session，证明 bridge 启动、input 被消费、
Claude session/init message 和 provider 的 `authentication_failed` 终态。录制环境 OAuth
过期，因此它不是成功 turn 证据，也没有证明真实 permission、QuestionSet、interrupt 或 resume。

这些路径可以有精确 reducer/bridge 测试，但在取得真实 SDK 成功运行前保持未验收。R5 不得因
provisional hostd 启动成功或 mock frame 测试而标为完成。

## 被否决的方案

| 方案 | 原因 |
|---|---|
| Rust 手写 Claude SDK DTO | 脱离官方类型检查，造成静默漂移 |
| 读取 `~/.claude` transcript | 历史文件不是实时/控制合同，且扩大敏感数据面 |
| PTY/TUI/屏幕抓取 | 明确违反产品边界 |
| `ready` 时伪造 raw Claude session id | 污染 provider binding，无法诚实 resume |
| 第一条 prompt 前没有 canonical Session | hostd 无稳定 runtime/session 路由目标 |
| 把 provisional Session 标为 live/history available | 把 Broker 生命周期冒充 provider 证据 |
| 把 SDK `listSessions()` 结果标为 `ExternalNative` | 冒充独立 CLI/GUI ownership/attach；应使用 `ProviderManaged` |
| 把 SDK managed session 称为原生 GUI attach | 改写 ADR-0009/R7 验收语义 |

## 后果

- Claude adapter 的上游类型漂移由官方 SDK typecheck 守卫，Rust 只维护自有 sidecar frame；
- `StructuredLanHost` 可以在没有首条 prompt 时与其他 provider 同时启动 Claude runtime；
- 移动端可看到一个诚实的 provisional Session，但 live/history/控制按钮仍由证据决定；
- SDK auth 失败是可展示的真实 provider 失败，不是成功路径；
- 成功 turn、真实 history read、permission/question、interrupt、resume、跨平台 CI 与实体 Android 仍是
  T-112/T-113 的未完成门禁。
