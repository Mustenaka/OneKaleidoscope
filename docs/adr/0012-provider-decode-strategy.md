# ADR-0012: Codex app-server 解码采用「钉定路径 + 必需面漂移守卫」

- 状态：**已接受，2026-07-30**
- 决策人：项目主管
- 修订：[ADR-0005](0005-schema-normalization-layer.md) D-1 对 Codex 解码路径的适用范围；`AGENTS.md` §3.2 的「上游类型一律生成」
- 触发：R1 合同定稿需要确定 T-100 的 Codex 解码方式

## 背景

[ADR-0005](0005-schema-normalization-layer.md) 已经实测记录：

- `typify 0.7.0` 无法消化 Codex 完整 schema，只取 v2 子集会丢掉 `ServerRequest`、
  三类 approval 请求和 `JSONRPCMessage`——恰好是本项目最需要的部分；
- `progenitor 0.14.0` 不支持 OpenCode 的 OpenAPI 3.1.0。

ADR-0005 D-2 允许插入规范化层，但同时规定「规则超过 10 条即视为该生成链不健康」，
并在「影响的门禁」中预留了出口：**若某家最终只能人工维护类型子集，必须另开 ADR
明确记录该妥协及其漂移检测手段。** 本 ADR 行使该出口。

M1 停滞的直接原因之一，就是把「先把三家上游类型全部生成出来」当成写第一行产品代码的
前置。[REQUIREMENTS](../REQUIREMENTS.md) §4.3 和 `CLAUDE.md` §4 已明确禁止这种排序。

同时，2026-07-30 录到的三份真实 Codex fixture 证明：本项目实际消费的上游面非常窄，
而且形状已经有一手证据（见本文件「已取证的解码面」）。

## 决策

### D-1 Codex 解码走「钉定路径解码」，不生成上游类型

`kaleido-adapter-codex` 不定义任何与 `schemas/codex` 同名的 Rust 类型（继续受 A-6 约束），
也不引入 typify/progenitor 生成链。它的解码器：

1. 只在 adapter 边界内把上游报文当作未定型 JSON 处理；
2. 只读取一份**显式声明的 JSON Pointer 清单**；
3. 立刻把读到的值转换为 `kaleido-proto` 的 canonical 类型；
4. 未定型 JSON 不得越过 adapter 边界进入 canonical、state 或移动端。

「手写上游类型」仍然是打回项——本决策不是允许手写，而是**不产生上游类型**。

### D-2 每条被读取的路径必须登记并被守卫

清单登记在 `crates/kaleido-adapter-codex/src/surface.rs` 的常量表中，每条含：

- canonical 用途；
- 上游 method 名；
- JSON Pointer；
- 对应 [`schemas/required-surface.toml`](../../schemas/required-surface.toml) 的 `entries.id`。

必须有一个测试遍历该表并断言：

1. 每条 pointer 都能在 `schemas/codex` 的**已提交原样快照**中解析到定义；
2. 每条 pointer 归属的 entry 在 `required-surface.toml` 中确实存在；
3. 表中不存在未被解码器实际使用的死条目。

清单之外的路径一律读不到。想读新字段，必须先加登记项并让守卫通过。

### D-3 未知与变形报文的处理

- 未登记的 method：计入诊断计数，产出 `StateEffect::DiagnosticRecorded`，安全忽略；
- 已登记 method 但 pointer 解析失败或类型不符：产出 canonical 错误
  （`ErrorCode::RuntimeProtocolViolation`），**不得静默降级为成功**；
- 两种情况都不允许 panic，也不允许伪造成已支持的投影。

这条是「静默漂移」的真实防线：生成的类型只在编译期报错，而运行期形状变化仍需要
上面的显式失败路径。

### D-4 适用范围仅限 Codex app-server

本决策只授权 Codex app-server 的解码路径。它不是三家 provider 的通用解码策略。
OpenCode 与 ACP/Claude 接入时，各自单独评估：若届时存在可用的生成链，优先生成；
若同样不可用，必须另开 ADR 记录，不能直接援引本 ADR。

`schemas/` 原样快照、`xtask schema diff` 与 `schemas/VERSIONS.md` 的漂移监控职责不变，
仍是升级 runtime 前的强制步骤。

## 已取证的解码面

下列形状来自本仓库 `tests/fixtures/codex/` 的真实录制，不是 schema 推断：

| 用途 | 上游 method | 证据 |
|---|---|---|
| 会话创建 | `thread/start` 响应 `result.thread.id` | `01-simple-turn.jsonl:6` |
| turn 建立 | `turn/start` 响应 `result.turn.id` / `status` | `01-simple-turn.jsonl:14` |
| 运行状态 | `thread/status/changed` `params.status.type` | `01-simple-turn.jsonl:15`、`:38` |
| 流式文本 | `item/agentMessage/delta` | `01-simple-turn.jsonl:30`～`:34` |
| item 生命周期 | `item/started` / `item/completed` | `01-simple-turn.jsonl:27`～`:35` |
| 审批请求 | `item/fileChange/requestApproval` `params.itemId` | `03-permission-approve.jsonl:50` |
| 审批回复 | `{"result":{"decision":"accept"}}` | `03-permission-approve.jsonl:51` |
| 拒绝终态 | item `status: "declined"` 且 turn 仍 `completed` | `04-permission-deny.jsonl:53`、`:84` |

## 后果

- T-100 不再依赖任何生成链，也不再等 T-005 的工具结论；
- `AGENTS.md` §3.2 的表格对 Codex 一栏改为「不生成，按本 ADR 钉定路径解码」；
- 新增的漂移守卫测试是 T-100 的 DoD 项，不是可选项；
- 若守卫表增长到难以维护（经验阈值：单 provider 超过 60 条），主管重新评估是否值得
  再试生成链，而不是继续无限加条目。

## 被否决的方案

| 方案 | 否决理由 |
|---|---|
| 继续按 ADR-0005 写规范化规则直到 typify 能吃下完整 Codex schema | 已实测撞到嵌套 `definitions` 与 `ServerRequest` 两处未实现分支；规则数会立刻越过 ADR-0005 的 10 条健康线 |
| 手写 Codex 上游类型子集 | 违反 A-6 与 `AGENTS.md` §3.2 核心原则，且产生一份没人对照 schema 校验的影子协议 |
| 让 canonical 层直接持未定型 JSON | 违反 [ARCHITECTURE](../ARCHITECTURE.md) INV-3 与 UniFFI 可表达性（[PROTOCOL](../PROTOCOL.md) §2 R-P1） |
| 推迟 T-100 直到三家生成链都可用 | 正是 M1 停滞的成因，`CLAUDE.md` §4 明确禁止 |
