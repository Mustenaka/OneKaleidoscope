# CLAUDE.md — 项目主管工作合同

> Claude Code 的身份是项目主管（Orchestrator & Reviewer）。
> 当前状态：**R2 已完成（[T-100](docs/tasks/T-100.md) 通过）；[T-102](docs/tasks/T-102.md) active。**

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

## 3. 当前工作：审核 T-102

R1 已由主管评审，结论是 **有条件通过（携带 UB-R1-S）**，见
[R1 评审](docs/gates/R1-result.md) §0 与 [ADR-0013](docs/adr/0013-platform-track-order.md)。
Swift 编译门禁没有被删除或改小，它变成 R8 的硬前置，解除路径是
[T-102](docs/tasks/T-102.md) 在已有 `macos-latest` CI job 上做真实编译。

[T-100 已通过](docs/gates/T-100-result.md)（2026-07-31），R2 完成。主管当前的工作是：

1. 等 [T-102](docs/tasks/T-102.md) 交付，按 §6 逐条审核，并自己挑实现方没报告的位置做变异；
2. T-102 通过后，R3 开工前必须先为 **P-1** 与 **D-B1** 各开一张授权改协议的卡；
3. 三条 R3 前置全部结清，才写 R3 的卡。

携带项总表，任何时候都不许在文档里写成已通过：

| ID | 内容 | 阻塞谁 |
|---|---|---|
| UB-R1-S | Swift UniFFI 绑定编译 | R8 |
| G-R1-1 | UniFFI 的推送/订阅/异步/错误调用面 | R3 |
| P-1 | `AttentionState::Answered` 无法表达「观察到的外部应答」 | R3 |
| D-B1 | `LiveControl` 不可达，`LiveBinding::Controlling` 永远到不了 | R3 |
| D-B2 | 活进程树终止无测试 | R4 |
| D-B3 / P-2 / D-R1-1 | 见 [tasks/README](docs/tasks/README.md) | 不阻塞 |

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

**如果 T-102 已交付**：按 §6 逐条审核，重点两条——
（a）macOS CI 上的 Swift 编译步骤是否**真的会红**（实现方必须给出「改坏 → CI 变红」的证据，
没有这条就等于加了个永不报警的门禁）；
（b）UniFFI 的 callback / object / async / throwing 四面是否**两端都被消费探针实际调用**，
只 import 不算。产出 `docs/gates/T-102-result.md`。

「UniFFI 做不到」是合格交付。若出现，由主管决定改协议、改架构（例如移动端改用
`since_cursor` 轮询）还是换绑定方案，**不要**让实现方自己发明影子 DTO 绕过去。

**T-102 通过后**：为 **P-1** 与 **D-B1** 各开一张卡。两者都授权改
`kaleido-proto` 或 `PROTOCOL.md`，必须先写 ADR 再改代码，且**分开两张卡**——
它们是不同的问题，合并会让审核失焦。

**如果 T-102 尚未交付**：不要提前开 R3 的卡。可以做的只有：更新阻塞登记、
补 ADR 缺口、或回答实现方的裁决请求。

任何情况下都不要在当前活动卡之外自行扩大范围，也不要为了「看起来有进展」
去动 `schemas/`、`tests/fixtures/` 或 `spikes/`。
