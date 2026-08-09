# MILESTONES — 重新定基线后的实施顺序

> 生效：2026-07-30（R1 主管评审后更新）
> 原 M1 与 T-001～T-014 已冻结/撤销。R1 **有条件通过**（携带 UB-R1-S）；
> [T-100](tasks/T-100.md) 已 active，R2 开工。平台顺序见
> [ADR-0013](adr/0013-platform-track-order.md)：Windows + Android 先行。

## 总原则

每个里程碑交付一个可操作的端到端纵切，不再以 schema 数量、fixture 格子或 recorder 完整度为进度。
每个能力必须同时有成功路径、错误路径、重连路径和真实 runtime 证据。

## R0 — 文档重新定基线

状态：**本次完成**

- 固定不可降级产品需求；
- 确定 Session Broker、canonical state、跨 Agent workflow；
- 确定自有 Ubuntu 协调/relay 是 v1 组件；
- 冻结旧任务与数据提取路线；
- 记录六个 provider × surface 验收格。

门禁：需求、架构、ADR、里程碑和主管交接说明之间无活动冲突。

## R1 — 合同定稿（项目主管负责）

状态：**有条件通过（携带 UB-R1-S），2026-07-30 主管评审**。验收记录见
[R1-result.md](gates/R1-result.md) §0。协议、proto、Kotlin 编译与 workspace 基线已恢复
并经主管独立复跑与变异复验；Swift 绑定已生成，但当前 Windows/WSL 环境没有 Swift 编译器。

按 [ADR-0013](adr/0013-platform-track-order.md) D-2，原「移动端双端编译」门禁拆为：

- **R1-K**（Kotlin 编译）：通过；
- **R1-S**（Swift 编译）：未通过，登记 **UB-R1-S**，**携带至 R8 作为硬前置**，
  解除路径是 [T-102](tasks/T-102.md) 在已有 `macos-latest` CI job 上的真实编译。

产出：

- `docs/PROTOCOL.md`；
- `crates/kaleido-proto`；
- canonical state、command、projection、cursor、capability、workflow 合同；
- Swift/Kotlin 的最小 UniFFI 编译探针；
- T-100 起的新任务卡（R1 未通过时保持 blocked）。

门禁：

- 合同能表达项目索引、历史、活动会话、队列、Attention Inbox、工作流 —— 通过；
- 明确 queue 与 steer、history 与 live、decline 与 error —— 通过；
- 用现有真实 fixture 验证至少两个 reducer 难点，但不要求补齐矩阵 —— 通过（三个）；
- 移动端绑定对最小真实类型编译通过 —— Kotlin 通过；Swift 见 UB-R1-S。

禁止：在 R1 前恢复 T-014 或创建 adapter 自有临时全局模型。

## R2 — 单 Provider 本地纵切

状态：**已完成，2026-07-31**。任务卡 [T-100](tasks/T-100.md)，两阶段交付均通过。
门禁结果见 [T-100-result.md](gates/T-100-result.md)，阶段 A 评审见
[T-100-stage-a-review.md](gates/T-100-stage-a-review.md)。

优先以 Codex Broker 管理的 app-server 会话完成：

```
provider → reducer → canonical state → durable log → Rust diagnostic client
```

验收：

- 项目与会话列表；
- 一次流式 turn；
- 工具生命周期与 approve/deny；
- plan/diff/状态至少按真实能力呈现；
- queue、steer、interrupt 的能力差异诚实可见；
- 进程重启后恢复状态；
- 未知消息和 join 失败有错误路径。

这一步只做一个 provider，证明架构纵切；不先并排写三套 adapter。

## R3 — Android 局域网纵切

状态：**进行中，2026-08-09**。旧入口前置 G-R1-1、P-1、D-B1 已分别由
T-102、T-103、T-104 解除。

R3 开工审计发现 projection cursor、可信 mobile command/content ingress 与 LAN 配对合同
仍需闭合，因此执行顺序固定为：

1. [T-106](tasks/T-106.md)：UACP 0.3 + TRANSPORT 0.1 合同；
2. [T-107](tasks/T-107.md)：hostd LAN broker + Rust mobile core；
3. [T-108](tasks/T-108.md)：Android Compose 与设备级纵切。

产出最小 hostd + Android App：

- 配对；
- ProjectIndex、SessionIndex、Transcript、LiveActivity；
- InputQueue、AttentionInbox；
- approve/deny、question、prompt/steer/interrupt；
- LAN 断线重连。

门禁：用户离开 PC 后可以只用 Android 完成一次会话干预。

## R4 — 自有 Ubuntu 远程连接

产出：

- rendezvous、P2P 连接信息交换；
- relay 密文回退；
- 设备注册、吊销与推送；
- E2EE 与日志/推送脱敏；
- 网络切换和 PC 离线状态。

门禁：家宽 PC ↔ 蜂窝网络手机连续运行，直连失败时自动 relay；服务器无法解密采样载荷。

NAT 20 轮测试属于性能与容量数据，不再决定 relay 是否开发，也不阻塞 R1～R3。

## R5 — 三家 Broker 管理会话

依次加入 OpenCode、Claude Code：

- OpenCode server REST + SSE；
- Claude Agent SDK streaming、sessions、permissions；
- ACP 作为额外兼容路径。

每加入一家，都必须复用相同 canonical state 和移动端投影，不能复制一套 provider 专属 UI。

门禁：三家各完成流式 turn、历史、等待人工、断线恢复；缺失能力由 runtime capability 明示。

## R6 — 跨 Agent 工作流

完成：

- workflow DAG 与持久化；
- Claude 规划 → Codex 执行 → Claude 审核；
- Artifact 交接；
- 人工 gate、返工、重试、取消和重新指派；
- Android WorkflowBoard 和关联会话跳转。

门禁：用户离开 PC 后，只用手机将一次真实工作从计划推进到审核完成。

## R7 — 原生 CLI/GUI 六格闭环

逐格验证 [REQUIREMENTS.md](REQUIREMENTS.md) §8：

- 优先使用 provider 公开的共享 runtime、attach、订阅或插件机制；
- 每格保留版本、启动方式、runtime 身份和端到端录屏/fixture；
- 上游没有公开路径时登记阻塞，不得用 PTY、磁盘轮询或产品文案绕过。

本里程碑可与 R3～R6 的 provider 工作并行研究，但 v1 最终验收不能跳过。

## R8 — iOS 对齐

**入口前置：UB-R1-S 必须先解除**（[ADR-0013](adr/0013-platform-track-order.md) D-2、
[T-102](tasks/T-102.md) §5.1）。R1 携带的 Swift 编译门禁在这里结清，不得跳过。

在 Android 核心交互稳定后对齐 iOS：

- 相同的共享核心与 projection；
- APNs 冷启动恢复；
- Keychain、后台限制、Biometric；
- 功能和错误语义与 Android 一致。

## R9 — 编辑器预览、Git 与发布硬化

最后加入：

- 文件树、搜索、只读代码预览；
- diff、Git status/stage/commit/push；
- Windows/macOS/Linux 打包与服务生命周期；
- 安全审计、恢复演练、性能和发布。

编辑器不是前序里程碑的阻塞项。

## 任务规则

- 新任务编号从 **T-100** 开始。
- 每张卡只属于一个纵切，必须写清对应 projection、command 和真实验收。
- fixture 只在需要证明具体语义时补录，不能创建“先录完再开发”的总门禁。
- 外部原生 GUI/CLI 的研究任务和 Broker 管理会话实现任务分开，避免一个未知阻塞全项目。
- 任何修改 `kaleido-proto` 的任务必须先由主管更新协议/ADR。
