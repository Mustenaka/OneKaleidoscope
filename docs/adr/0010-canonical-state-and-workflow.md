# ADR-0010: 规范化状态与跨 Agent 工作流是一等模型

- 状态：**已接受，2026-07-30**
- 决策人：项目负责人
- 取代：固定“12 个 SessionEvent”作为 UACP 核心的设计，以及 ADR-0007 D-3

## 背景

现有真实 fixture 已经证明 provider 报文不是移动端事件的 1:1 来源：

- Codex 审批请求需要与先前 Item join 才有可展示上下文；
- `decline` 是 Item 正常终态，不能映射成 Turn Error；
- plan、task、diff、tool、message 经常是同一对象的不同阶段；
- 手机需要的是可恢复的项目、会话、队列、等待项和工作流状态，而不是上游报文列表。

同时，项目负责人的核心场景要求 Claude 规划、Codex 执行、Claude 审核。这不是三个互不相关的聊天，
而是一个有依赖、产物和人工 gate 的持久化工作流。

## 决策

### D-1 UACP 以状态、命令和投影为核心

provider 报文必须经过 decoder 和 reducer，形成规范化状态转移，再产生移动端 projection。
事件日志用于重放状态转移，不把任意上游事件直接暴露给移动端。

### D-2 固定 12 事件模型退出当前合同

旧 12 事件清单保留为历史研究线索，不再限定 proto、不再要求三家矩阵覆盖，也不再用作里程碑。
具体事件/投影集合由 `PROTOCOL.md` 从移动端读模型和真实 reducer 需要推导。

### D-3 四类状态分离

- Session runtime status；
- Agent plan/tasks；
- User input queue；
- Attention Inbox。

queue 消息只有 provider 明确确认注入活动 turn 后才能变成 steer；否则保持 queued。

### D-4 Workflow 是 v1 必做

Workflow/Step/Artifact/HumanGate 进入 canonical model 与 proto。v1 至少支持：

- 手工创建或导入 plan；
- provider/role 指派；
- 依赖、完成条件和产物交接；
- 人工 gate；
- retry、rework、cancel、reassign；
- 手机查看并推进。

自动选择模型、自动评估质量可以后续增强，但不能把工作流本身推迟到 v2。

### D-5 Reducer 测试按语义取证

只有需要证明具体语义时才补录 fixture。每个纵切至少有一条真实成功路径、一条错误/拒绝路径和一条恢复路径。
不得为了填满矩阵制造或手写“理想上游报文”。

## 后果

- T-014 的临时本地状态模型在实施前撤销；
- 项目主管先定义 `PROTOCOL.md` 与 `kaleido-proto`；
- 移动端可以围绕稳定 projection 开发，不随 provider 事件改名；
- 跨 Agent 工作流有明确的持久化和审计边界。
