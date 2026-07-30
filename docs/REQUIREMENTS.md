# OneKaleidoscope — 产品需求基线

> 状态：**v2，项目负责人确认，2026-07-30**
> 本文件是需求真源。技术困难、上游缺口和阶段性实现不得降低最终需求。

## 0. 产品定义

OneKaleidoscope 是一个面向个人开发者的跨设备 Agent 控制平面：

- PC 上的 Claude Code、Codex、OpenCode 继续在本机项目中执行；
- 手机按 provider、项目和会话查看历史与实时状态；
- 手机可以审批、回答、排队、引导、取消和继续任务；
- 多个 Agent 可以组成“规划 → 执行 → 审核 → 返工”的工作流；
- 数据来自公开的结构化协议，不来自终端画面转发。

典型场景是：用户在 PC 上预先编排 plan 并开始执行，然后离开桌面；之后只用手机观察进度、
处理等待项、修正方向并完成跨 Agent 交接。

## 1. 不可降级目标

| ID | 目标 | 验收含义 |
|---|---|---|
| OBJ-1 | 三家 Agent | Claude Code、Codex、OpenCode 均有真实端到端实现 |
| OBJ-2 | CLI + 原生 GUI | 每家都分别验证 CLI 与官方/原生 GUI 表面，共六个验收格 |
| OBJ-3 | 实时查看 | 进行中 turn 的状态和增量在手机可见，不用会话结束后落盘冒充实时 |
| OBJ-4 | 实时控制 | 手机可 prompt、queue、steer、approve/deny、answer、interrupt、retry |
| OBJ-5 | 项目与历史 | 按 provider → 项目 → 会话分类；过去和正在进行的会话都可查看 |
| OBJ-6 | 任务与队列 | 明确区分运行状态、Agent 计划/任务、用户输入队列和等待人工处理项 |
| OBJ-7 | 跨 Agent 编排 | 一个工作流可让不同 provider 分别规划、执行、审核，并从手机推进 |
| OBJ-8 | 协议直连 | 禁止 PTY/TUI/ANSI/屏幕抓取作为 Agent 输出来源 |
| OBJ-9 | 私有连接 | PC 与手机端到端加密；自有 Ubuntu 服务器不能读取业务明文 |
| OBJ-10 | 跨平台 | PC 支持 Windows、macOS、Linux；手机支持 Android、iOS |

任何六格验收中的失败都必须明确显示为“未实现”或“受上游阻塞”，不得隐藏按钮后宣布完成。

## 2. 产品对象与界面

移动端的主层级是：

```
Host
└── Provider
    └── Project
        ├── Workflow
        │   └── Step
        └── Session
            └── Turn
                └── Item
```

### 2.1 项目首页

必须提供：

- 按 Claude Code / Codex / OpenCode 分类与跨 provider 的“全部项目”视图；
- 项目在线状态、当前运行数、等待人工数、失败数；
- 最近会话、历史会话和归档会话；
- 跨 Agent 工作流及其当前步骤。

### 2.2 会话页

必须显示：

- 历史消息和当前流式消息；
- 推理/状态摘要、工具调用生命周期、diff、计划与任务更新；
- 当前状态：idle / running / waiting_user / waiting_approval / queued / failed / completed；
- 待发送输入队列，支持编辑、排序、删除；
- 当前运行时的真实能力与限制。

必须提供：

- 新消息；
- 进行中 steer；若 runtime 不支持，必须明确进入队列，不可假装已注入；
- approve / deny、结构化问题回答；
- interrupt、retry、resume；
- 从会话创建工作流步骤或把结果交给下一 Agent。

### 2.3 工作流页

跨 Agent 工作流是 v1 必做，不是未来占位。最低能力：

- 有向步骤与依赖关系；
- 每一步指定 provider、角色、项目/worktree、输入和完成条件；
- 产物显式交接：plan、diff、提交、审查意见、测试结果；
- 状态：draft / ready / running / blocked / waiting_human / review / rework / completed / failed / cancelled；
- 人工 gate、重试、返工回路、跳过和重新指派；
- 手机可查看每一步关联会话并执行上述干预。

自动决策可以后做，但工作流状态与手工推进不能后做。

## 3. PC 端：Session Broker

`kaleido-hostd` 是本机控制平面，不是终端代理。它负责：

1. 发现公开的 provider runtime 与历史存储入口；
2. 启动并拥有 Broker 管理的会话；
3. 在上游公开允许时附着外部原生表面的实时会话；
4. 把 provider 事件归约为规范化状态，再生成移动端投影；
5. 保存项目、会话、工作流、队列、等待项和 cursor；
6. 执行配对、传输、断线重放和推送唤醒；
7. 提供只读文件树、文件预览、diff 与后续 Git 操作。

一次性安装/启用 OneKaleidoscope 集成是可接受的产品前提。它可以安装 daemon、launcher、
插件或配置共享 server，但不能依赖未公开的厂商 Remote Control 私有接口。

## 4. Provider 接入原则

### 4.1 三种会话模式

每个会话必须标注模式，不能压成一个 `attach` 布尔值：

| 模式 | 含义 |
|---|---|
| `broker_managed` | Broker 通过公开协议创建并拥有运行时 |
| `shared_runtime` | 原生表面与 Broker 都是同一公开 server 的客户端 |
| `external_native` | 原生表面独立创建；只有存在公开实时订阅/控制接口时才可附着 |

历史来源与实时运行时是两个独立字段。能列出或恢复历史，不代表能订阅另一个进程当前的 turn。

### 4.2 能力按 runtime 协商

协议至少需要表达以下能力，实际名称由 `PROTOCOL.md` 定稿：

```
history.list/read/resume
live.observe/control/multi_subscriber
turn.prompt/steer/interrupt/retry
interaction.approval/question
state.plan/tasks/diff/tool_lifecycle
queue.read/write/reorder
workflow.participate
```

UI 只能按连接后得到的能力渲染，禁止按 provider 名称或版本号硬编码。

### 4.3 当前公开路径

| Provider | 首选公开路径 | 当前判断 |
|---|---|---|
| Codex | app-server JSON-RPC | 适合 Broker 管理的会话；外部 Codex Desktop 活动会话的发现与绑定尚无稳定公开合同 |
| Claude Code | Claude Agent SDK | 适合 Broker 管理的流式会话、恢复和审批；官方 Remote Control 仅供 Anthropic 自有客户端，不作为集成接口 |
| OpenCode | server REST + SSE | 最接近原生共享 runtime；CLI/TUI 和具体 GUI 都必须分别做端到端实测 |
| ACP | 兼容层 | 用于 ACP Agent 或必要桥接，不作为所有 provider 的最低公分母 |

不得通过完整 schema 能否生成来决定产品能力；只对已经进入当前纵切的必需面生成/维护类型。

## 5. 规范化合同

UACP 的核心不是“固定 N 个事件枚举”，而是：

```
Provider message
    → provider decoder
    → reducer
    → canonical state transition
    → durable log
    → mobile projection
```

`docs/PROTOCOL.md` 和 `crates/kaleido-proto` 必须覆盖：

- Host、ProviderRuntime、Project、Workflow、Step、Session、Turn、Item；
- 状态快照与增量投影；
- 命令、确认、幂等键、错误与 capability；
- 会话级 cursor、断线重放、快照压缩与背压；
- 审批/问题的相关对象和过期语义；
- 队列与进行中 steer 的严格区分；
- 内容引用，避免将完整文件、diff、工具参数写入普通日志。

真实 fixture 用于证明 reducer 语义和回归测试，不要求在写产品代码前收齐所有 provider 的所有事件。

## 6. 自有 Ubuntu 服务器与连接

Ubuntu 服务是 v1 的正式组件，不再由 NAT 成功率决定是否开发。它负责：

- 设备注册与一次性配对；
- rendezvous / discovery；
- P2P 直连失败时的加密 relay；
- APNs / FCM 唤醒转发；
- 可选的加密离线信封和最小在线状态。

连接策略是局域网直连 → 公网 P2P → 自有 relay。直连率测试用于优化体验和容量，不再是产品开工门禁。

硬性安全要求：

- 业务内容在 PC/手机端加解密，服务器没有内容密钥；
- relay、推送和普通日志中不得出现消息、代码、diff、工具参数、完整路径、token；
- 配对可撤销，命令可鉴权，审批决定必须绑定会话、请求和过期时间；
- hostd 不暴露无鉴权明文公网端口；
- PC 断电或离线时，手机必须诚实显示离线，服务器不得替代 Agent 执行。

## 7. v1 优先级

### 7.1 必做

1. Android 上的项目/会话/历史/实时状态/队列/审批/引导。
2. 三家 Broker 管理会话的结构化协议纵切。
3. 六个原生表面验收格的证据与阻塞状态。
4. 跨 Agent 工作流的创建、观察、人工 gate 与交接。
5. 自有 Ubuntu 协调与 relay、E2EE、断线重放、推送。
6. iOS 功能对齐。

### 7.2 次优先但保留

- 项目文件树；
- 只读代码预览；
- diff 查看；
- Git status / stage / commit / push。

这些功能不能再次抢占会话实时性、进行中任务、队列和跨 Agent 工作流的实现顺序。

### 7.3 明确不做

- 终端转发 escape hatch；
- 逆向厂商私有 Remote Control 协议；
- 云端替用户运行 Agent；
- 自建 LLM 推理；
- 多用户团队权限体系；
- 在移动端重写 provider 协议或核心状态机。

## 8. 验收矩阵

每个 provider × surface 都要保存真实证据：

| 场景 | Claude CLI | Claude GUI | Codex CLI | Codex GUI | OpenCode CLI | OpenCode GUI |
|---|---:|---:|---:|---:|---:|---:|
| 历史可列出与打开 | 待验收 | 待验收 | 待验收 | 待验收 | 待验收 | 待验收 |
| 活动 turn 实时可见 | 待验收 | 上游阻塞待复查 | 待验收 | 上游阻塞待复查 | 待验收 | 待验收 |
| 手机控制并同步 | 待验收 | 上游阻塞待复查 | 待验收 | 上游阻塞待复查 | 待验收 | 待验收 |
| 断线后状态/队列一致 | 待验收 | 待验收 | 待验收 | 待验收 | 待验收 | 待验收 |

“上游阻塞”不是通过。解除条件必须是公开文档、公开协议或可重复的真实端到端证明。

## 9. 成功标准

产品达到 v1 的最低演示必须是：

1. PC 上建立一个 Claude 规划 → Codex 执行 → Claude 审核的真实工作流；
2. 用户离开 PC，只用 Android 查看各步骤和关联会话；
3. 手机处理一次权限请求、一次问题、一次 steer 或排队消息、一次审核返工；
4. 网络切换后恢复，状态、队列和等待项不丢不重；
5. 同样的核心功能在 iOS 对齐；
6. 六格验收矩阵没有被隐藏或伪造。
