# ADR-0014: Codex 审批族按回复合同分别建模；turn 时间戳是秒

- 状态：**已接受，2026-07-31**
- 决策人：项目主管
- 修订：[PROTOCOL.md](../PROTOCOL.md) §11.1 映射表两处；[T-100](../tasks/T-100.md) §3.3
- 触发：T-100 阶段 A 交付时实现方对着已提交 schema 快照发现映射表有误
- 不改动：`crates/kaleido-proto` 的任何类型

## 背景

[PROTOCOL.md](../PROTOCOL.md) §11.1 原来把三个审批 method 写成同一行：

```
| `item/fileChange/requestApproval`     | AttentionUpserted（Approval，join 按 params.itemId 解析） |
| `item/commandExecution/requestApproval` | 同上 |
| `item/permissions/requestApproval`      | 同上 |
```

这一行是**主管在没有核对回复 schema 的情况下写的**。实现方在 T-100 阶段 A 建漂移守卫时
把它否掉了，主管随后对着 `schemas/codex/` 的已提交快照复核，确认实现方正确：

| 回复类型 | `required` | 形状 |
|---|---|---|
| `FileChangeRequestApprovalResponse` | `["decision"]` | `decision` 是字符串枚举 `accept` / `acceptForSession` / `decline` / … |
| `CommandExecutionRequestApprovalResponse` | `["decision"]` | `decision` 的 `oneOf` 除字符串分支外还含「应用 execpolicy 修订」等分支，与 fileChange **不同构** |
| `PermissionsRequestApprovalResponse` | `["permissions"]` | **根本没有 `decision`**，必须回一个 `GrantedPermissionProfile` |

三者的 params 确实相似，但**回复合同不同**。把它们压成一个 `AttentionItem` 并给出
accept/decline 两个选项，等于给手机用户提供了 runtime 从未提供的选项——对
`permissions` 一族更是直接错误。这正是 [PROTOCOL.md](../PROTOCOL.md) §4.7 禁止的
「客户端硬编码同意/拒绝」。

同时，实现方在真实录制里发现 `turn/started` 与 `turn/completed` 的
`startedAt` / `completedAt` 是 **Unix 秒**（`01-simple-turn.jsonl` 中为 `1785378397`），
而同一批报文里 item 与审批的 `startedAtMs` 是毫秒。R-P2 要求 canonical 一律毫秒。
主管已复核 fixture 确认。

## 决策

### D-1 审批按「回复合同」而不是「参数形状」分族

v0.1 只建模 **file-change** 一族——它是本仓库唯一有一手回复录制的一族。
`item/commandExecution/requestApproval` 与 `item/permissions/requestApproval`
归入 `DiagnosticRecorded { UnknownUpstreamMessage }`，直到：

1. 仓库里有该族的真实回复录制；且
2. 主管为它定义了对应的 `AttentionSubject` 与选项集合。

在此之前它们**不得**渲染成审批项。手机上少一个可操作项，好过给出一个 runtime 不接受的按钮。

这不是能力隐藏：缺口通过诊断计数可见，并且 `interaction_approval` 能力的证据来源
（`RecordedFixture`）已经说明了它的依据范围。

### D-2 上游 turn 时间戳按秒解释，adapter 负责换算

`turn/started` / `turn/completed` 的 `startedAt` / `completedAt` 是 Unix 秒，
adapter 必须 ×1000 转成 canonical 毫秒。item 与审批的 `*Ms` 字段本来就是毫秒，不换算。
两者混在同一批报文里，所以这条必须写进映射表，不能靠实现方记忆。

### D-3 两处已登记但本 ADR 不修的协议缺口

| ID | 缺口 | 处置 |
|---|---|---|
| P-1 | `AttentionState::Answered` 强制携带 `command_id`，但 replay 里的决定来自**被录制的客户端**，本地从来没有过对应的 `CommandEnvelope`。实现方铸了一个确定性 ID 代表「被观察到的决定」 | 接受为阶段 A 的权宜之计（已在代码注释写明）。**R3 开工前**必须改协议：`Answered` 需要能区分「本地命令应答」与「观察到的外部应答」。改 `kaleido-proto` 要单独开卡授权，不得夹带进 T-100 |
| P-2 | Codex 审批请求不携带过期时间，`expires_at_ms` 恒为 `None`，`ApprovalExpired` 在真实流量里永远触发不了 | 这是上游事实，不是缺陷。记录在案：移动端必须能正确渲染「无过期时间」的审批，不得假定所有审批都有倒计时 |

P-1 属于 canonical 状态里存在一个指不到任何命令的引用。它现在被 replay 路径圈住，
影响可控；但 R3 之后手机会真的按 `command_id` 回查，所以必须在那之前结清。

## 后果

- [PROTOCOL.md](../PROTOCOL.md) §11.1 映射表两处更新；
- [T-100](../tasks/T-100.md) §3.3 从「三个 method → AttentionUpserted」改为「仅 file-change 族」；
- 阶段 A 的实现**不需要返工**——它已经按正确的形状做了，本 ADR 是把文档追平到证据；
- P-1 进入 R3 前置清单，与 G-R1-1 并列。

## 教训

映射表里凡是写「同上」的行，都要单独核对过上游合同才能写。这一行是纸面推断，
被漂移守卫抓住——这正是 [ADR-0012](0012-provider-decode-strategy.md) D-2 设置该守卫的目的。
