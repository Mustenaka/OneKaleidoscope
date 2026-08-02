# OneKaleidoscope 当前状态

> 生效日期：2026-07-31（T-100 门禁评审后更新）
> 状态：**R2 完成。[T-100](tasks/T-100.md) 两阶段均通过；下一张卡是 [T-102](tasks/T-102.md)**

R0 通过，R1 经主管评审后**有条件通过**：
[R0](gates/R0-result.md)、[R1](gates/R1-result.md)。

唯一携带项是 **UB-R1-S：Swift UniFFI 绑定编译**。按
[ADR-0013](adr/0013-platform-track-order.md)，项目负责人确认 v1 的平台推进顺序是
**Windows + Android 先行，macOS + iOS 后置**，因此：

- Swift 编译门禁**不删除、不改小**，改为 **R8（iOS 对齐）的硬前置**；
- 解除路径已确定且不需要负责人拥有 Mac：`.github/workflows/ci.yml` 的
  `macos-latest` job 增加一步真实 `swiftc` 编译，见 [T-102](tasks/T-102.md)；
- R2（纯 Rust 本机纵切）与 R3（消费已编译通过的 Kotlin 绑定）都不产生 Swift 代码，
  因此不被该项阻塞。

**[T-100](tasks/T-100.md) 已于 2026-07-31 通过**（[门禁结果](gates/T-100-result.md)、
[阶段 A 评审](gates/T-100-stage-a-review.md)）。R2 达成：真实 Codex app-server 会话经
钉定路径 decoder → reducer → canonical state → durable log → 六个投影，
可观察、可干预、可重启恢复。

**[T-102](tasks/T-102.md) 已于 2026-08-02 通过**（[门禁结果](gates/T-102-result.md)）。
UB-R1-S 与 G-R1-1 解除：Swift 在 macOS CI 上真实编译，UniFFI 的 callback / object /
async / throwing 四面两端消费并编译。

**当前活动：仓库收敛**（[prompt](tasks/repo-convergence-prompt.md)）。分支是 `ae9da23` 的
直系后代，可快进；差异恰好是 `.gitattributes` 与 12 个主管文档。

收敛完成后依次下发 R3 的两条硬前置，**分开两张卡**：
[T-103](tasks/T-103.md)（P-1）→ [T-104](tasks/T-104.md)（D-B1）。

## 1. 当前结论

项目不是“完全不能做”，但旧路线把问题错误地定义成了“从任意已经运行的进程里提取完整数据”，
因此不断陷入 schema、fixture、进程发现和事件覆盖率的拉锯。

现在采用的路线是：

> **OneKaleidoscope 在 PC 上提供 Session Broker。它通过各家公开的结构化协议拥有或连接会话，
> 将会话状态、历史、进行中任务、审批、队列和跨 Agent 工作流投影到手机。**

终端 PTY、ANSI/TUI 抓屏、窗口文字识别、轮询 transcript 冒充实时协议，均不属于实现路径。

## 2. 不可降级的最终产品

最终产品必须同时满足：

1. 支持 Claude Code、Codex、OpenCode。
2. 支持每家的 CLI 与原生 GUI 表面。
3. 手机按 provider → 项目 → 会话查看历史和正在进行的会话。
4. 手机实时看到文本、推理、工具、计划、任务、diff、审批、问题和运行状态。
5. 手机可发送新消息、排队、steer、批准/拒绝、回答问题、取消与重试。
6. 支持 Claude Code 规划 → Codex 执行 → Claude Code 审核等跨 Agent 工作流。
7. 不通过终端转发获取 Agent 数据。
8. 编辑器/代码预览保留在产品范围，但不是当前纵切重点。

“公开接口暂时做不到”只会形成阻塞记录，不会改写上述目标，也不能算门禁通过。

## 3. 已确认与尚未确认

| Provider | Broker 管理的结构化会话 | 历史 | 外部原生 CLI 实时附着 | 外部原生 GUI 实时附着 |
|---|---|---|---|---|
| Codex | app-server 可支撑 | `thread/list/read/resume` 可支撑 | 需对共享实例做端到端证明 | **未发现稳定公开的 Desktop 发现/绑定合同** |
| Claude Code | Agent SDK 可支撑流式会话、审批和恢复 | SDK 可列出/读取/恢复 | 独立 CLI 进程没有公开第三方实时附着合同 | 官方 Remote Control 只面向 Anthropic 自有客户端；第三方接口不可依赖 |
| OpenCode | server REST + SSE 可支撑 | session/message API 可支撑 | `attach` + SSE 路径明确，仍需实录验收 | 需证明具体 GUI 版本连接同一 server |

这些结论按“具体 runtime 实例的能力”验证，不能按 provider 名称硬编码，也不能把“能读历史”
写成“能实时接管”。

## 4. 当前仓库处置

- `crates/kaleido-proto`：**合同**。修改必须先改 [PROTOCOL.md](PROTOCOL.md) 并走 ADR。
- `tests/fixtures/`、`schemas/`、`spikes/`：保留为一手证据和研究资产，不再作为开始产品代码的前置门禁。
- T-001～T-013：冻结为历史任务。
- T-014：在实现前撤销；它绕过了尚未定稿的规范化状态合同。
- `docs/tasks/M1-queue.md` 与根目录旧 `KICKOFF.md`：删除，避免继续下发失效队列。
- 旧 ADR：保留决策历史；被新 ADR 取代的内容不得作为当前合同。
- `kaleido-recorder` 的历史脱敏占位符测试已按授权做窄范围修正，生产脱敏顺序和
  fixture 均未改变；workspace Rust 基线已恢复，主管已独立复跑 `cargo xtask ci`（exit 0）。
- UB-R1-S：本机没有 `swiftc` / `swift` / `xcrun`。Swift 绑定源码已生成
  （`kaleido_proto.swift` 357744 B），但不能伪报编译通过。见
  [R1 评审](gates/R1-result.md) §0 与 [T-102](tasks/T-102.md)。

## 5. 文档优先级

发生冲突时按以下顺序解释：

1. [REQUIREMENTS.md](REQUIREMENTS.md)
2. 已接受且未被取代的 ADR
3. [PROTOCOL.md](PROTOCOL.md) 与 `crates/kaleido-proto`
4. [ARCHITECTURE.md](ARCHITECTURE.md)
5. [MILESTONES.md](MILESTONES.md)
6. 新任务卡
7. 冻结任务、旧队列、fixture 说明与 spike 文档

历史文件可以证明事实，不能重新定义范围。

## 5.5 R2 已完成的内容（[T-100](tasks/T-100.md)，2026-07-31 通过）

四个新 crate，约 6800 行实现 + 2600 行测试：

| crate | 内容 |
|---|---|
| `kaleido-state` | canonical 状态、追加式 durable log、内容寻址存储、六个投影、命令幂等 |
| `kaleido-adapter` | provider 中立的 `ProviderRuntimeSession` trait 与能力探针类型 |
| `kaleido-adapter-codex` | 41 条钉定路径表 + 解码器 + reducer + stdio JSON-RPC 进程传输 |
| `kaleido-hostd` | 组合根与诊断客户端：`slice run` / `slice replay` / `slice show` |

已在真实 Codex `0.146.0` 上取得一手证据：流式 turn、审批 accept/decline、
steer 意图始终排队、进程退出三联状态、重启后从日志重建。
离线与实时**共用同一套 decoder / reducer**，差别只在 `EvidenceSource`。

## 6. R1 已完成的内容

1. [PROTOCOL.md](PROTOCOL.md) v0.1：canonical state、命令、投影、cursor、能力、错误、
   工作流，以及基于真实 fixture 的 Codex 映射附录。
2. `crates/kaleido-proto`：与协议逐字一致的最小合同，含把 R-P8 / R-P9 / R-P7 / R-P10
   写成校验器的实现，36 个覆盖全部必需变体与错误路径的契约测试。
   主管已用两处变异（含一处实现方未预告的）复验测试真实性。
3. [ADR-0012](adr/0012-provider-decode-strategy.md)：Codex 解码采用钉定路径 + 必需面漂移守卫，不走生成链。
4. `kaleido-core` 最小 UniFFI 门面直接导出 canonical 类型；Kotlin 绑定生成与编译通过，
   Swift 绑定生成通过、编译携带为 UB-R1-S。
5. [ADR-0013](adr/0013-platform-track-order.md)：平台推进顺序与 Swift 门禁的携带式处置。
6. [T-100](tasks/T-100.md)：第一张产品任务卡，**已 active**。

## 7. 尚未取得机器证据的项

| ID | 内容 | 归属 | 解除路径 |
|---|---|---|---|
| ~~UB-R1-S~~ | ~~Swift UniFFI 绑定编译~~ | — | **已解除，2026-08-02**，[T-102 门禁结果](gates/T-102-result.md) |
| ~~G-R1-1~~ | ~~UniFFI 的 callback / object / async / throwing 面~~ | — | **已解除**。结论：R3 投影推送**能**走 UniFFI 回调；但**未**证明线程调度、背压、进程恢复 |
| P-1 | `AttentionState::Answered` 强制 `command_id`，被观察到的外部应答无本地命令 | **R3 硬前置** | 需单独开卡授权改 `kaleido-proto`；见 [ADR-0014](adr/0014-codex-approval-families-and-timestamp-units.md) D-3 |
| D-B1 | `LiveControl` 无任何代码路径可证明，`LiveBinding::Controlling` 结构性不可达 —— 手机会永远渲染成只读 | **R3 硬前置** | 见 [T-100 门禁结果](gates/T-100-result.md) §4 |
| D-B2 | 活进程树终止无测试（只测了「已退出」分支） | R4 前置 | 同上 |
| D-B6 / D-B7 | 跨平台路径校验规则（Unix 上的 `\`、macOS `/var` 符号链接别名）未定 | **R9 前置** | 见 [ADR-0015](adr/0015-frozen-spike-tests-are-windows-only.md) D-4；写 R9 路径校验前先定规则 |
| D-B8 | 脱敏占位符优先级：`<HOME>` 先于 `<SANDBOX>`（非泄漏，标签精度问题） | R4 复查 | 同上 |
| D-B3 | 跨 stream 投影的 cursor 语义待确认 | 不阻塞 | 同上 |
| P-2 | Codex 审批无过期时间，`ApprovalExpired` 真实流量永不触发 | 不阻塞 | 移动端需正确渲染「无过期时间」 |

G-R1-1 是主管评审时新发现的：现有探针只证明 canonical **数据类型**可被两端表达，
没有证明「投影推送到手机」这条通路可以走 UniFFI 回调。这个答案影响
[ARCHITECTURE](ARCHITECTURE.md) §9 的模块边界，必须在 R3 开工前拿到。

## 8. 下一步

1. **下发 [T-102](tasks/T-102.md)**（prompt：[T-102-codex-prompt.md](tasks/T-102-codex-prompt.md)）：
   在已有 `macos-latest` CI job 上取得真实 Swift 编译证据（UB-R1-S），
   并探明 UniFFI 的 callback / object / async / throwing 面（G-R1-1）；
2. T-102 通过后，R3 开工前还要结清 **P-1** 与 **D-B1** —— 两者都要改
   `kaleido-proto` 或协议，必须单独开卡授权，不得夹带；
3. 然后才进入 R3（Android 局域网纵切）。

仍未解除的上游阻塞：Codex Desktop 与 Claude Code 原生 GUI 的第三方实时附着
（[REQUIREMENTS](REQUIREMENTS.md) §8 两格），归 R7，不阻塞上述顺序。
