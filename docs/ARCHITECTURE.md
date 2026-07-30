# ARCHITECTURE — OneKaleidoscope v2

> 状态：**重新定基线，2026-07-30**
> 需求真源是 [REQUIREMENTS.md](REQUIREMENTS.md)，当前执行状态见 [STATUS.md](STATUS.md)。

## 1. 架构结论

OneKaleidoscope 是控制平面，不是终端镜像：

```
Native CLI / GUI ─┐
Broker launcher ──┼── Public provider protocol ──► Session Broker
Mobile command ───┘                                  │
                                                     ├─ Canonical state + reducers
                                                     ├─ Workflow engine
                                                     ├─ Durable log + projections
                                                     └─ E2EE transport
                                                               │
                                      Android / iOS ◄── P2P / own relay
```

核心转变是：先定义手机真正需要的状态，再从 provider 协议归约到这些状态。不能再从“抓到多少事件”
倒推产品模型。

## 2. 六条不变量

| ID | 不变量 |
|---|---|
| INV-1 | Agent 数据只来自公开结构化协议；PTY/TUI/ANSI/屏幕/窗口文字不进入数据路径 |
| INV-2 | 历史来源与实时运行时分离；能恢复历史不等于能附着活动 turn |
| INV-3 | provider 报文先归约成规范化状态，再产生 UI 投影；不是 1:1 事件改名 |
| INV-4 | 每个可见状态都可由快照 + cursor 后的日志重建；断线后不丢不重 |
| INV-5 | 能力属于具体 runtime 连接；UI 不按 provider 名字或版本分支 |
| INV-6 | 自有服务器只协调和中继密文，不拥有 Agent、不执行任务、不读取业务内容 |

## 3. Session Broker

### 3.1 会话所有权

Broker 支持三种模式：

- `broker_managed`：Broker 创建并拥有 provider runtime。
- `shared_runtime`：Broker 和原生 CLI/GUI 连接同一公开 server。
- `external_native`：独立原生表面创建；只有公开协议允许实时订阅和控制时才能附着。

每个 session 都要记录：

- `history_source`：历史由哪个公开 API/store adapter 提供；
- `live_runtime_id`：当前实际执行的 runtime；
- `ownership_mode`；
- `capabilities`；
- `connection_state` 与最后一次能力探测证据。

不能通过读磁盘 transcript、猜窗口标题或扫描 PID 将 `external_native` 伪装成实时会话。

### 3.2 Provider 策略

| Provider | Broker 管理路径 | 共享/外部表面策略 |
|---|---|---|
| Codex | app-server JSON-RPC；Thread → Turn → Item | 只在公开的同实例发现/订阅机制被端到端证明后声明原生 CLI/GUI 附着 |
| Claude Code | Agent SDK 流式会话、permissions、session resume/list | 官方 Remote Control 不是第三方接口；独立原生表面当前只承诺公开能力可证明的部分 |
| OpenCode | server REST 命令 + global/session SSE | 优先把原生 TUI/GUI 与 Broker 接到同一 server；分别验收 |
| ACP | 兼容适配层 | 用于 ACP-compatible agent，不强迫三家都降到 ACP 交集 |

## 4. 规范化领域模型

```
Host
├── ProviderRuntime
├── Project
│   ├── Session*
│   └── Workflow*
├── Workflow
│   └── Step* ──► Session / Artifact / HumanGate
└── AttentionItem*

Session
└── Turn
    └── Item
```

### 4.1 Session 状态

Session 自身只表达运行状态，不混入任务清单：

```
offline | idle | running | waiting_user | waiting_approval
queued | failed | completed | cancelled
```

Turn 与 Item 有各自生命周期。审批拒绝是 Item 的正常终态，不自动等于 Turn 失败。

### 4.2 三类容易混淆的状态

必须独立建模：

1. **运行状态**：Agent 是否正在工作、离线、等待或失败。
2. **Agent plan/tasks**：Agent 对当前任务的分解及完成度。
3. **用户输入队列**：尚未注入 runtime 的 prompt/steer；可编辑、排序和取消。

第四类 `AttentionItem` 聚合等待人工处理的 approval、question、review gate 和连接故障，
供手机全局 Inbox 使用。

### 4.3 工作流

Workflow 是跨 Agent 的持久化有向图，不是聊天文本里的约定。Step 至少包含：

- provider/runtime 选择；
- role（plan / implement / review / verify / custom）；
- 输入 Artifact 引用；
- 输出合同与完成条件；
- 依赖与人工 gate；
- 重试策略、返工目标和关联 Session；
- 审计记录：谁在何时从什么状态推进到什么状态。

Broker 只能在依赖、能力和 gate 均满足后调度 Step。手机和 PC 使用同一组工作流命令。

## 5. Decoder、Reducer、Projection

数据路径：

```
Provider message
  → generated/minimal decoder
  → provider reducer
  → canonical state transition
  → write-ahead durable log
  → subscriber-specific projection
```

- decoder 负责上游形状和前向兼容；
- reducer 负责 join、生命周期和不变量；
- durable log 保存规范化状态转移或命令结果；
- projection 面向会话列表、transcript、live activity、queue、inbox、workflow 等 UI。

真实 fixture 的作用是证明特定 reducer 语义。例如现有 Codex fixture 已证明：

- 审批请求可能只有 `itemId`，必须与已有 Item join；
- `decline` 是 Item 终态，不等于 Turn error；
- diff 是敏感大载荷，日志和推送只保存内容引用。

未知 provider 消息必须保留诊断计数并安全忽略或降级，不能 panic；但也不能被伪造成已支持的投影。

## 6. Durable State 与重放

写路径遵循：

```
validate command
  → provider accepted / local transition
  → assign monotonic cursor
  → durable append
  → publish projection
```

需要同时支持：

- 周期性快照，避免长会话从零回放；
- `since_cursor` 增量同步；
- 客户端幂等命令键；
- projection 版本；
- 慢客户端背压和重连；
- 内容寻址大载荷，正文与元数据分离。

“飞行模式前后逐字节相同”不是合理的状态验收；正确口径是相同命令序列收敛到相同规范化状态，
且 cursor 无缺口、无重复应用。

## 7. 移动端读模型

核心读模型至少有：

| Projection | 用途 |
|---|---|
| `ProjectIndex` | provider/项目分类、在线与计数 |
| `SessionIndex` | 历史、活动、归档、运行状态 |
| `Transcript` | Turn/Item、文本、工具、diff 引用 |
| `LiveActivity` | 当前增量、进度、计划和任务 |
| `InputQueue` | 待发送消息、顺序、可编辑状态 |
| `AttentionInbox` | 审批、问题、审核 gate、错误 |
| `WorkflowBoard` | 步骤依赖、角色、产物、返工 |
| `RuntimeCapabilities` | 当前连接能做什么及原因 |

Swift/Kotlin 只渲染这些读模型并发命令；协议、状态机和 provider 逻辑必须位于共享核心。

## 8. 连接与服务器

```
Mobile ── LAN direct ───────────────► hostd
   └──── public P2P, rendezvous ────► hostd
   └──── Ubuntu relay (ciphertext) ─► hostd
```

Ubuntu 服务从 v1 开始就是必需的可靠性后备，不以打洞率阈值决定是否存在。P2P 仍为首选，
relay 是现实移动网络下的确定性回退。

控制面包含配对、吊销、在线信号、push token 和 relay 路由；数据面端到端加密。
离线信封若实现，也只能保存有上限、有过期时间的密文。

## 9. 初步模块边界

名称可在 `PROTOCOL.md` 后细化，但依赖方向必须保持：

```
kaleido-proto              canonical types and commands
      ↑
kaleido-state              reducers, workflow, projections
      ↑
kaleido-provider           provider-neutral runtime traits
      ↑
kaleido-provider-*         Codex / Claude / OpenCode
      ↑
kaleido-hostd              composition root and local services

kaleido-transport ─────────► kaleido-hostd / kaleido-core
kaleido-core ──────────────► UniFFI ─► Android / iOS
kaleido-relay               independent ciphertext relay
```

上游生成类型只存在于对应 provider crate，不能穿透到 canonical 或移动端。

## 10. 明确禁止

- 为了“看起来通了”转发终端或 tmux；
- 把 transcript 文件轮询称为实时；
- 按 provider 名字隐藏功能缺口；
- 先实现三家完整 schema，再写产品纵切；
- 用固定事件数量当架构完成度；
- 在 proto 未定稿前创建 adapter 自有的临时全局模型；
- 让服务器持有 provider 凭据、项目文件或业务明文；
- 把“手机消息已排队”显示成“已 steer 到当前 turn”。

## 11. 仍需协议定稿的内容

项目主管必须在 `docs/PROTOCOL.md` 决定：

- 全部对象 ID、快照、cursor、命令确认和错误模型；
- canonical state transition 与 projection 的边界；
- workflow/step/artifact/human gate；
- queue 与 steer；
- approval/question 的关联与过期；
- capability 证据和变化通知；
- 内容引用、脱敏和保留策略；
- UniFFI 可表达性。

这些问题解决前，没有产品实现任务卡处于活动状态。
