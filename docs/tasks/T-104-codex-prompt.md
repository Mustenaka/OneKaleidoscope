# T-104 下发给 Codex 的 prompt（冷启动版，原样粘贴）

> **T-103 通过后**再下发。粘贴横线以内的全部内容。

---

你是 OneKaleidoscope 项目的**实现方（Implementer）**。仓库在
`D:\Work\Code\Cross\OneKaleidoscope`，Windows。你的交付会被逐条审核，
审核者会自己挑你没报告过的位置把实现改坏，验证测试是否真的变红。

**这张卡的主体不是写代码，是回答一个语义问题。** 代码量可能很小，
但如果语义定错了，R3 的手机端会建立在错误前提上。

## 第一步：读

| 文件 | 为什么 |
|---|---|
| `AGENTS.md` | 行为规范 |
| `docs/STATUS.md` | 当前状态与全部携带阻塞项 |
| `docs/tasks/T-104.md` | **本次任务卡，§5 的 DoD 是唯一验收标准** |
| `docs/PROTOCOL.md` §4.2 | `Capability`、`CapabilityState`、`CapabilityEvidence`、`EvidenceSource` |
| `docs/PROTOCOL.md` §4.3 | `LiveBinding`、`validate_against`、R-P7 |
| `docs/PROTOCOL.md` §6.1 | `CommandOutcome::AcceptedLocally` vs `AcceptedByRuntime` —— **这个区分很可能就是答案的一半** |
| `docs/REQUIREMENTS.md` §4.1 / §4.2 | 三种会话所有权模式；能力按 runtime 协商 |
| `docs/adr/0009-session-broker.md` | D-2 所有权模式、D-4 能力五态 |
| `docs/gates/T-100-result.md` §4 | D-B1 是怎么被发现的 |
| `crates/kaleido-adapter/src/capability.rs` | `CapabilityProbe` 的当前形状 |
| `crates/kaleido-adapter-codex/src/reduce.rs` | `live_binding()` 与能力提升的位置 |

## 问题

`Capability::LiveControl` 在 `crates/kaleido-adapter/src/capability.rs` 里
**只出现在 `ALL_CAPABILITIES` 枚举列表中，没有任何代码路径会把它标记为 proven**。

而 `PROTOCOL.md` §4.3 规定：**`Controlling` 要求同时具备 `LiveObserve` 与 `LiveControl`。**

所以 `LiveBinding::Controlling` **结构性不可达**。

但 T-100 阶段 B 的真实运行里，broker 确实通过这条 live 连接提交了 prompt、
回答了审批，runtime 都接受了。真实投影长这样：

```text
live_observe    supported      observed_in_traffic
turn_prompt     supported      observed_in_traffic
live_control    not_verified   absent               ← 明明控制了，却说没验证
```

**手机要靠 `LiveBinding::Controlling` 判断「我现在能不能干预」。不解决，手机永远只读。**

## 阶段一：先回答语义问题，写 ADR（做完停下来等批准）

在写任何代码之前回答：**`LiveControl` 到底是什么？它和 `TurnPrompt` /
`InteractionApproval` 有什么区别？**

至少有两种可信读法：

| 读法 | 含义 | 后果 |
|---|---|---|
| **A** | 「能对这条 live 连接发出任何改变状态的命令」，是 `TurnPrompt` 等的上位概括 | 任一控制类命令被 runtime 接受即 proven |
| **B** | 「能控制一个**不是自己创建**的会话」，针对 `shared_runtime` / `external_native` 的附着控制 | `broker_managed` 天然不需要它 → `Controlling` 对 R2 不适用，那手机靠什么判断可干预？ |

现有协议文本没写清楚——**这是主管的缺口，不是你的**。ADR 的核心工作就是做这个决定。

`docs/adr/0019-*.md` 必须回答：

1. `LiveControl` 的确切定义，及其与 `TurnPrompt` / `InteractionApproval` 的关系；
2. 什么样的观察构成 `LiveControl` 的证据
   （提示：`CommandOutcome::AcceptedByRuntime` 比 `AcceptedLocally` 更接近正确答案）；
3. `Controlling` 在三种 `OwnershipMode` 下分别意味着什么；
4. **R3 的手机靠哪个字段决定是否显示干预按钮** —— 这条必须有明确答案。
   如果你选读法 B，更要回答清楚，不能留下「协议自洽但手机没法用」的结果；
5. 如果结论是协议文本要改，给出 §4.2 / §4.3 的具体改法。

写完停下来。**不要开始阶段二。**

## 阶段二：实现（批准后）

改 `crates/kaleido-adapter` 与 `crates/kaleido-adapter-codex`，
以及 `docs/PROTOCOL.md`（如果 ADR 决定要改）。

## 最容易翻车的地方

**为了让 `Controlling` 可达而放宽 `LiveBinding::validate_against` 或 `EvidenceSource`
的判定。** 那等于用「让它能到达」替换「证明它该到达」，是本项目最严重的一类违规。

正确做法是：**先定义清楚什么算证据，再让证据真的被观察到。**

三条硬要求，都有测试：

1. **replay 路径仍然到不了 `Controlling`** —— 录制证明协议形状，不证明现在能控制；
2. **没有证据时到不了** —— 仅有 `live_observe` 时 `Controlling` 必须被拒；
3. **`turn_steer` 仍然是 `not_verified`** —— 不许被这次改动顺带提升。

## 真实验收

用 T-100 已有的 `slice run` 跑一次真实 Codex 会话（`0.146.0`，工作目录用一次性的
`target/t104-scratch/`），贴出 `SessionIndexView` 与 `RuntimeCapabilityView` 的实际 JSON，
证明 `live_control` 与 `LiveBinding` 的取值符合 ADR 的定义。

## 边界

不许改 `docs/REQUIREMENTS.md`、`docs/ARCHITECTURE.md`、`schemas/**`、
`tests/fixtures/**`、`spikes/**`。

不许顺手动 P-1（T-103 已处理）或 D-B2 / D-B6 / D-B7 / D-B8 / D-B11 ——
它们都是已登记的未修复项，文档里也不许写成已解决。

不许扩大到 workflow、transport、移动端、其他 provider。

## 允许的结论

如果 ADR 阶段的结论是「`LiveControl` 这个能力本身设计有问题，应该合并 / 拆分 / 删除」，
**那也是合格交付**。停下来写清楚，由主管决定。

## DoD 摘要（完整版在任务卡 §5）

ADR 先于代码并获批准；`LiveControl` 有真实可达的证据路径且只在真控制后成立；
replay 到不了 `Controlling`；无证据到不了；真实验收 JSON；`turn_steer` 未被提升；
至少三处「改坏 → 变红」含**无 runtime 接受证据就提升 `LiveControl`**；
`cargo xtask ci` exit 0 且三平台 CI 全绿。

现在开始阶段一。**只写 ADR，不动代码。**
