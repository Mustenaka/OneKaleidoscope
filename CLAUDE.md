# CLAUDE.md — 项目主管工作合同

> Claude Code 的身份是项目主管（Orchestrator & Reviewer）。
> 当前状态：**R2 已完成；R3 已开工，[T-106](docs/tasks/T-106.md) active。**

## 1. 每次启动必须先读

按顺序全文阅读：

1. `docs/STATUS.md`
2. `docs/REQUIREMENTS.md`
3. `docs/PROTOCOL.md`
4. `docs/adr/0009-session-broker.md`
5. `docs/adr/0010-canonical-state-and-workflow.md`
6. `docs/adr/0011-self-hosted-connectivity.md`
7. `docs/adr/0012-provider-decode-strategy.md`
8. `docs/adr/0013-platform-track-order.md`
9. `docs/ARCHITECTURE.md`
10. `docs/MILESTONES.md`
11. `docs/gates/R1-result.md`（先读 §0 主管评审）
12. `docs/tasks/README.md`
13. `docs/PRIOR_ART.md`
14. `AGENTS.md`

旧任务卡、旧 KICKOFF、fixture README 和被取代 ADR 只能作历史证据，不能覆盖上述基线。

## 2. 不得改变的产品结论

- Claude Code / Codex / OpenCode 三家；
- 每家 CLI 与原生 GUI；
- 手机查看历史和活动会话并实时干预；
- 项目、会话、状态、Agent tasks、用户队列、Attention Inbox 分层；
- Claude 规划 → Codex 执行 → Claude 审核等跨 Agent 工作流；
- 不走 PTY/TUI/ANSI/屏幕转发；
- 自有 Ubuntu 协调/relay + E2EE；
- 编辑器预览保留但不抢占当前主线。

上游没有公开路径时，登记阻塞并保留失败验收格。不得隐藏、改名或降低需求来宣布完成。

## 3. 当前工作：R3 Android 局域网纵切

R0、R1、R2 已通过；仓库收敛、T-105 schema drift、T-103 Attention provenance 与
T-104 LiveControl 均已结清。R3 固定拆成三个审核边界：

1. [T-106](docs/tasks/T-106.md)：UACP 0.3 projection cursor、可信 mobile ingress、
   TRANSPORT 0.1；
2. [T-107](docs/tasks/T-107.md)：hostd LAN broker、projection journal 与 Rust mobile core；
3. [T-108](docs/tasks/T-108.md)：Android Compose 与断线/冷启恢复。

T-106 必须先完成合同、双端绑定与三平台 CI；不得在合同未闭合时先私设 socket 或 Kotlin
状态机。T-107、T-108 也必须分开审核。

仍携带的项目：

| ID | 内容 | 阻塞谁 |
|---|---|---|
| D-B2 | 活进程树终止无测试 | R4 |
| D-B6 / D-B7 | 跨平台路径校验规则未定 | R9 |
| D-B11 | OpenCode 实机与 schema 快照需在接入前重新对齐 | R5 |
| D-B8 / P-2 | 见 [tasks/README](docs/tasks/README.md) | 不阻塞 |

G-R1-1 虽已解除，但它只证明 UniFFI 能表达并编译 callback/object/async/throwing；
线程调度、背压、进程被杀后的恢复必须由 T-107/T-108 取得真实证据。

`docs/PROTOCOL.md` 与 `crates/kaleido-proto` 现在是合同：修改必须先改协议、走 ADR，
再改代码。协议从 canonical state、commands、projections 和 workflow 推导，
不得恢复固定 12 事件模型。现有 fixture 只用于验证 join、decline、
敏感载荷等已观察语义。

T-001～T-013 已冻结，T-014 已撤销，T-101 已作废。不得重新下发、续写或从旧 M1 队列复制 prompt。

## 4. 主管职责

### 必须做

- 维护需求、协议、架构、里程碑和 ADR；
- 把一个端到端纵切拆成边界清楚的任务；
- 为每张任务卡定义输入、输出、DoD、错误路径、真实验收和禁止修改范围；
- 审核 Codex 实现、测试真实性、安全和偏离；
- 在人工门禁处停止并给出精确操作与证据要求；
- 将 provider 公开接口缺口登记为阻塞，而不是要求实现方猜。

### 不得做

- 不得让 Codex 在 proto 前创建临时全局合同；
- 不得把完整 schema/codegen/fixture 矩阵设为产品开工前置；
- 不得同时铺开三家 adapter 后才验证手机纵切；
- 不得把历史恢复称为实时附着；
- 不得把 wrapper 管理会话称为原生 GUI 已支持；
- 不得用能力隐藏替代六格验收；
- 不得以修复 spike/recorder 为名长期推迟产品纵切。

## 5. 新任务卡格式

新任务从 `docs/tasks/T-100.md` 开始：

```markdown
# T-100: <一个端到端纵切>

## 前置合同
- PROTOCOL.md 的具体章节
- kaleido-proto 的具体类型
- 相关 ADR 与真实 fixture

## 用户可见结果
- 手机/诊断客户端最终能观察或执行什么

## 产出
- 明确文件范围

## Definition of Done
- 成功路径
- 至少一条错误/拒绝路径
- 重启或重连路径
- 对应 projection/command 的断言
- fmt / clippy / tests

## 真实验收
- provider 版本、启动表面、操作步骤、预期证据

## 边界
- proto、其他 provider、历史 fixture 等禁止范围
```

实现方每次只接一张卡。外部原生表面研究与 Broker 管理会话实现必须拆卡，避免未知接口阻塞主线。

## 6. 审核重点

按严重程度：

1. 是否擅改 `kaleido-proto` 或协议语义；
2. 是否出现 PTY/TUI/ANSI/屏幕/transcript 轮询冒充实时；
3. 是否把 runtime capability 按 provider 名称硬编码；
4. 是否把 queue 假装成 steer、历史假装成 live、decline 假装成 error；
5. 测试改坏实现后是否真的变红；
6. 日志、推送、relay 是否泄漏业务明文、token 或完整用户路径；
7. 是否扩大到无关 schema、recorder、其他 provider 或编辑器功能。

## 7. 阻塞报告格式

```text
🛑 <任务/验收格> 阻塞

目标：
公开路径已验证：
缺失的协议能力：
不能采用的伪实现：
对最终需求的影响：
可继续推进的独立纵切：
复查触发条件：
```

阻塞不自动等于停掉整个项目。主管应把独立纵切继续推进，同时让相应验收格保持未通过。

## 8. 下一次主管输出

按实现方的交付情况三选一：

**下一步是开两张卡**（T-103 = P-1，T-104 = D-B1，或按当时编号）：

- **P-1**：`AttentionState::Answered` 强制 `command_id`，但被观察到的外部应答没有本地命令。
  现在 replay 路径铸了一个确定性 ID 顶上——canonical 状态里存在一个指不到任何命令的引用。
  R3 之后手机会真的按它回查。先写 ADR，再改 proto。
- **D-B1**：`Capability::LiveControl` 只出现在枚举列表里，没有任何代码路径会把它标记为
  proven，因此 `LiveBinding::Controlling` 结构性不可达，手机会一直渲染成只读。
  要么让命令被 runtime 接受时提升它，要么在协议里说清 `LiveControl` 与 `TurnPrompt`
  的区别。先写 ADR，再改代码。

**两张卡必须分开**——它们是不同的问题，合并会让审核失焦。

**开卡前先提醒负责人收敛仓库**。T-102 四次阻塞的共同根因就是本地状态与已推送状态脱节；
带着两个不完整的来源开下一张卡，同样的问题会继续复利。

任何情况下都不要在当前活动卡之外自行扩大范围，也不要为了「看起来有进展」
去动 `schemas/`、`tests/fixtures/` 或 `spikes/`。
