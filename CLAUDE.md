# CLAUDE.md — 项目主管工作合同

> 进入仓库的 Claude Code、Codex 与人类遵循同一执行角色；具体技术纪律见 [AGENTS.md](AGENTS.md)。
> 当前状态：**R0～R3 已完成；下一里程碑是 R4 自有 Ubuntu 远程连接。**

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

## 3. 当前工作：R3 已结清，准备 R4

R0、R1、R2 已通过；仓库收敛、T-105 schema drift、T-103 Attention provenance 与
T-104 LiveControl 均已结清。R3 最终由四个独立边界完成：

1. [T-106](docs/tasks/T-106.md)：UACP 0.3 projection cursor、可信 mobile ingress、
   TRANSPORT 0.1；
2. [T-107](docs/tasks/T-107.md)：hostd LAN broker、projection journal 与 Rust mobile core；
3. [T-108](docs/tasks/T-108.md)：Android Compose 与断线/冷启恢复。
4. [T-109](docs/tasks/T-109.md)：实体 arm64 Wi-Fi、硬件密钥、真实审批、OEM 后台、
   force-stop cursor 恢复与吊销门禁。

T-106～T-109 均已通过。R4 尚未开卡；开卡前先审计 Ubuntu rendezvous/relay、跨公网
E2EE、推送与活进程树终止的现有合同和实现缺口。

仍携带的项目：

| ID | 内容 | 阻塞谁 |
|---|---|---|
| D-B2 | 活进程树终止无测试 | R4 |
| D-B6 / D-B7 | 跨平台路径校验规则未定 | R9 |
| D-B11 | OpenCode 实机与 schema 快照需在接入前重新对齐 | R5 |
| D-B8 / P-2 | 见 [tasks/README](docs/tasks/README.md) | 不阻塞 |

G-R1-1 已解除；线程调度、背压、进程被杀后的恢复也已由 T-107～T-109 取得 emulator
与实体设备证据。

`docs/PROTOCOL.md` 与 `crates/kaleido-proto` 现在是合同：修改必须先改协议、走 ADR，
再改代码。协议从 canonical state、commands、projections 和 workflow 推导，
不得恢复固定 12 事件模型。现有 fixture 只用于验证 join、decline、
敏感载荷等已观察语义。

T-001～T-013 已冻结，T-014 已撤销，T-101 已作废。不得重新下发、续写或从旧 M1 队列复制 prompt。

## 4. 统一执行职责

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

任务必须保持可独立审核的边界；互不冲突的模块可以并行。外部原生表面研究与 Broker
管理会话实现仍必须拆卡，避免未知接口阻塞主线。

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

阻塞不自动等于停掉整个项目。执行者应把独立纵切继续推进，同时让相应验收格保持未通过。

## 8. 下一次输出

下一步为 R4 开独立任务卡。先做只读审计并明确：Ubuntu host 身份与部署、rendezvous、
relay 密文路径、跨公网 E2EE、设备推送、网络切换、活进程树终止及隐私日志门禁。
不得把 LAN TLS 直接称为公网 relay，也不得为推进 R4 擅改 `schemas/`、历史 fixture 或其他
provider。仍需保留 R5 的 OpenCode D-B11、R7 的原生 CLI/GUI 六格和 R9 的路径规则边界。
