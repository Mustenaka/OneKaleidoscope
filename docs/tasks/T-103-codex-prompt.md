# T-103 下发给 Codex 的 prompt（冷启动版，原样粘贴）

> 仓库收敛完成后再下发。粘贴横线以内的全部内容。

---

你是 OneKaleidoscope 项目的**实现方（Implementer）**。仓库在
`D:\Work\Code\Cross\OneKaleidoscope`，Windows。协议与架构由项目主管定义，
你的交付会被逐条审核，审核者会自己挑你没报告过的位置把实现改坏，验证测试是否真的变红。

**这张卡和之前所有卡有一个根本不同：它授权你修改 `crates/kaleido-proto` 和
`docs/PROTOCOL.md`。但必须先写 ADR、等主管批准，再动一行代码。**

## 第一步：读

| 文件 | 为什么 |
|---|---|
| `AGENTS.md` | 行为规范。§2 铁律、§4 交付格式、§5 阻塞报告格式 |
| `docs/STATUS.md` | 当前状态与全部携带阻塞项 |
| `docs/tasks/T-103.md` | **本次任务卡，§4 的 DoD 是唯一验收标准** |
| `docs/PROTOCOL.md` §4.7 | `AttentionItem`、`AttentionState`、`AttentionResponse`、`check_reply` |
| `docs/PROTOCOL.md` §6 / §6.1 | `CommandEnvelope`、`CommandAck`、`CommandOutcome` |
| `docs/PROTOCOL.md` §4.2 | `CapabilityEvidence` / `EvidenceSource` —— 已有的「证据来源」表达，先看能不能复用 |
| `docs/adr/0014-*.md` D-3 | P-1 的原始登记 |
| `docs/gates/T-100-stage-a-review.md` §3 | P-1 是怎么被发现的 |
| `crates/kaleido-proto/src/attention.rs` | 合同的代码形态 |
| `crates/kaleido-adapter-codex/src/reduce.rs` | 当前铸造 ID 的位置与它的注释 |

## 问题

`AttentionState::Answered` 强制携带 `command_id`：

```rust
Answered {
    option_id: Option<String>,
    free_form_ref: Option<ContentRef>,
    decided_at_ms: i64,
    command_id: CommandId,      // ← 强制
}
```

「手机发命令 → broker 应答审批」这条路径上它是对的。但还有第二条真实路径：
**Broker 观察到一个它没有发出的应答**。T-100 的 replay 就是——决定来自被录制的客户端，
本地从来没有过对应的 `CommandEnvelope`。

现在的实现铸了一个确定性 ID 顶上（注释里写明了）。主管当时接受为权宜之计并登记为 **P-1**。
现在要根治。

**为什么必须在 R3 前解决**：手机会按 `command_id` 回查是谁、什么时候做的决定。
一个指向不存在命令的 ID，在 R3 会变成 UI 上的空洞或错误。而且「铸个 ID 顶上」
一旦成为先例会扩散。

这条路径也不只有 replay 会走：R5 接入 `shared_runtime` 后，
原生 CLI/GUI 在同一个 server 上做的决定，Broker 同样只能观察到。

## 你要做的

### 阶段一：ADR（先做完，停下来等批准）

写 `docs/adr/0018-*.md`，必须回答：

1. `Answered` 如何区分「本地命令应答」与「观察到的外部应答」；
2. 观察到的应答携带什么证据（谁观察到、何时、来源）；
3. 为什么复用 / 不复用 `CapabilityEvidence` 的 `EvidenceSource`；
4. 对 `AttentionItem::check_reply` 的影响 —— 观察到的应答还能不能被本地再回复一次？
   （提示：不能，但错误码要选对，别硬塞一个语义不合的）；
5. 对已有 durable log 的影响。R2 已经产出过真实日志，**明确说明旧记录怎么处理**。
   协议还在 `0.1.x`，可以选择不兼容，但必须写清楚，不能含糊过去。

**如果你认为「加一个变体」不是最好的形状**——比如应该把 `command_id` 改成 `Option`、
或者引入独立的 `AnswerSource` 记录——**在 ADR 里提出你的方案并说明理由**。
ADR 阶段就是用来做这个决定的。不要先写代码再回头补 ADR。

写完停下来。**不要开始阶段二。**

### 阶段二：实现（批准后）

1. `docs/PROTOCOL.md` §4.7 与 `crates/kaleido-proto/src/attention.rs` 同步修改，
   **文档先于代码**，字段名逐字一致；
2. `crates/kaleido-adapter-codex/src/reduce.rs` **删掉铸造 ID 那段**，改用新表达。
   live 模式对合成 effect 的抑制逻辑一并重新评估——新表达可能让抑制变得不必要，
   如果是就删掉它并说明理由。

## 三条红线

**1. 授权范围只到 `AttentionState` 及其直接相关类型。** `kaleido-proto` 的其他类型
一个字段都不许动。交付时贴 `git diff --stat` 自证。

**2. 不许顺手动 D-B1。** `LiveControl` 不可达是 `docs/tasks/T-104.md`，独立一张卡。
同样不许动 D-B2 / D-B6 / D-B7 / D-B8 / D-B11 —— 它们都是已登记的未修复项，
在文档里也不许写成已解决。

**3. T-100 的既有语义不许被这次改动破坏。** replay 三份 fixture 后，
decline 仍是 Item 终态、Turn 仍 `completed`、`turn_steer` 仍 `not_verified`。
`slice run` 的真实路径里，本地发出的审批决定仍然要携带**真实**的 `command_id`。

## DoD 摘要（完整版在任务卡 §4）

- ADR 先于代码并获批准；
- `PROTOCOL.md` 与 proto 逐字段一致；
- 契约测试覆盖本地应答、外部观察应答、两者在 `check_reply` 下的差别；
- **错误路径**：对观察到的外部应答再发本地回复，返回语义正确的错误；
- `reduce.rs` 里不再有铸造的 `CommandId`，grep 自证；
- 至少三处「改坏 → 变红」，必须包含**把观察到的应答伪装成本地命令应答**；
- `cargo xtask ci` exit 0 且三平台 CI 全绿。

## 交付格式

按 `AGENTS.md` §4.2：DoD 逐条勾选、贴真实测试输出、`git diff --stat` 自证边界、
偏离说明、发现的问题、「改坏→变红」单独成节。

主管会自己挑你没报告的位置做变异复验。

现在开始阶段一。**只写 ADR，不动代码。**
