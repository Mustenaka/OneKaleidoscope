# R1 一次性恢复验收

- 日期：2026-07-30
- 执行方：Codex（Implementer）
- 评审：项目主管（Claude Code），2026-07-30 完成，见 §0
- 当前结论：**有条件通过（携带 UB-R1-S）**。协议、proto、Kotlin UniFFI 与整个 Rust
  workspace 已恢复、经主管复验；Swift 绑定生成成功但未编译，按
  [ADR-0013](../adr/0013-platform-track-order.md) D-2 拆为 R1-S 并携带至 R8。
- 任务状态：[T-100](../tasks/T-100.md) 已改为 **active**；[T-102](../tasks/T-102.md) 排队。

---

## 0. 主管评审（2026-07-30）

实现方的自检结论基本属实。主管**自己动手**做了以下复验，不是照抄交付说明：

### 0.1 独立复跑

```text
cargo xtask ci
<== fmt-check: ok
<== check-deps: ok
<== lint-forbidden: ok
<== clippy: ok
<== test: ok
<== fixtures-verify: ok; 5 file(s), 220 record(s) (codex: 3, acp-claude: 1, opencode: 1)
exit code 0
```

### 0.2 主管自己的变异验证（含一处实现方未报告的）

按 `CLAUDE.md` §6.5，重点是确认测试套件**不是只针对实现方自报的那五处**过拟合。
因此第二处刻意选了交付说明里没有提到的校验器：

```text
# 1) 复验实现方自报项：ItemStatus::is_failure 增加 Declined
test declined_item_is_terminal_but_not_a_failure_and_turn_can_complete ... FAILED
assertion failed: !ItemStatus::Declined.is_failure()
test result: FAILED. 35 passed; 1 failed

# 2) 实现方未报告项：AttentionItem::check_reply 的过期检查改成 if false && ...
test attention_reply_binds_target_session_key_state_expiry_and_offered_option ... FAILED
  left: Ok(())
 right: Err(Expired)
test result: FAILED. 35 passed; 1 failed
```

两处均已还原，`cargo test -p kaleido-proto` 恢复 **36 passed**。
结论：契约测试对未预告的改动同样变红，测试真实性通过。

### 0.3 抽查项

| 检查项（`CLAUDE.md` §6） | 结论 |
|---|---|
| 是否擅改 proto / 协议语义 | 无。proto 与 `PROTOCOL.md` 是本轮新建，改动均有 ADR 依据 |
| 是否有 PTY/TUI/ANSI/屏幕/轮询冒充实时 | 无。`kaleido-proto` 内无 IO、无 tokio、无 `serde_json::Value` 泄漏 |
| 是否按 provider 名称硬编码能力 | 无。A-2 豁免 0；能力走 `CapabilityState` 五态 |
| queue/steer、history/live、decline/error 是否混淆 | 三处均有独立校验器与错误路径测试 |
| 日志/推送是否泄漏 | `ContentRef` + §10 白名单在类型层面成立；有对应测试 |
| 是否扩大到无关范围 | 未动 `schemas/**`、fixture JSONL、`spikes/**` |
| Swift 绑定是否真的生成 | 是。`target/uniffi/swift/` 下 `kaleido_proto.swift` 357744 B、`kaleido_core.swift` 22538 B、两份 FFI header 与 modulemap |

### 0.4 主管发现的、实现方未报告的缺口

**G-R1-1：UniFFI 探针只覆盖值类型，未覆盖移动端真正的调用面。**

`kaleido_core::binding_probe` 是一个同步函数，参数与返回值都是 record。它证明了
canonical **数据类型**可被两端表达，但没有覆盖 R3 必需的三类构造：

1. callback interface —— 投影推送（`LiveActivityView` 增量、`AttentionInboxView` 新审批）；
2. throwing / async 函数 —— 命令失败与 `CommandAck` 在外语言侧的形态；
3. 有状态 object —— 订阅句柄、退订、冷启重建。

这三类若在 UniFFI 下不可用，改的是 [ARCHITECTURE](../ARCHITECTURE.md) §9 的模块边界，
不是 UI。已开 [T-102](../tasks/T-102.md) 覆盖，并规定**必须在 R3 开工前完成**。

### 0.5 判定

| 门禁项 | 结论 |
|---|---|
| R1 除 Swift 编译外的全部门禁 | **通过** |
| R1-K（Kotlin 绑定编译） | **通过**，842 class，`KOTLIN_PROBE_EXIT=0` |
| R1-S（Swift 绑定编译） | **未通过**，登记 UB-R1-S，携带至 R8 硬前置 |
| R1 总结论 | **有条件通过（携带 UB-R1-S）** |

依据是 [ADR-0013](../adr/0013-platform-track-order.md)：项目负责人确认平台顺序为
Windows + Android 先行、macOS + iOS 后置；R2 是纯 Rust 本机纵切，R3 消费的是已编译
通过的 Kotlin 绑定，两者都不产生 Swift 代码。UB-R1-S 没有被删除、改名或改小，
解除路径是 `.github/workflows/ci.yml` 已有的 `macos-latest` job（见 [T-102](../tasks/T-102.md)）。

**下发**：[T-100](../tasks/T-100.md) 改为 active，分两阶段交付，阶段 A 后有强制评审。

---

## 0.6 以下为实现方原始交付记录（保留，不修改）

本次没有修改 `schemas/**` 或任何 fixture JSONL；没有实现 adapter、reducer、hostd、
移动端、transport 或 relay；没有提交或推送。

## 1. 当前 R1 DoD

| 门禁 | 结论 | 证据 |
|---|---|---|
| `ProjectIndexView` | 通过 | [PROTOCOL](../PROTOCOL.md) §8 与 `projection.rs` 逐字段定义 `ProjectIndexView`、`ProviderGroup`、`ProjectSummary`、`ProjectBindingSummary` |
| `SessionIndexView` | 通过 | §8 逐字段定义 active/history/archived 与 binding/live/status/counts |
| `TranscriptView` | 通过 | §8 `TranscriptView` / `TranscriptTurn`；真实 fixture 证明 completion summary 不覆盖累积 item |
| `LiveActivityView` | 通过 | §8 active turn、streaming items、plan、tasks、更新时间 |
| `InputQueueView` | 通过 | §4.6 / §8；精确 pending-set reorder validator |
| `AttentionInboxView` | 通过 | §4.7 / §8；回复绑定 attention/session/request key/state/expiry/options |
| `WorkflowBoardView` | 通过 | §4.8 / §8；binding/worktree、依赖、能力、gate、六种人工动作 |
| `RuntimeCapabilityView` | 通过 | §4.2 / §8；缺失项为 `NotVerified`，五态与证据可见 |
| queue / steer | 通过 | 只有 `ObservedInTraffic` 且 runtime/session/turn/binding/current-turn/capability 全匹配才可 `DeliveredAsSteer` |
| history / live | 通过 | history 不推导 live；controlling 同时要求 live observe + control |
| decline / error | 通过 | `Declined` 是 Item 终态但不是 failure；deny fixture 的 Turn completed 且 error 为空 |
| cursor / snapshot / replay | 通过 | checked cursor；重复、gap、跨流、overflow 分别拒绝；Host/Project/Session/Workflow 四类快照收敛 |
| 幂等与背压语义 | 通过 | `CommandOutcome::Duplicate`、唯一权威 log cursor、`BackpressureCoalesced` / `CursorGap` 已定义 |
| ContentRef 正文读取 | 通过 | §4.10 鉴权内容查询；请求/响应、chunk 上限、offset overflow、continuation 与 unavailable 状态都有 validator |
| provider 原始 ID 隔离 | 通过 | canonical/log/projection 只接收 Broker `bnd_` handle；实体 kind 不能串用；原始 ID 留在 adapter 私有存储 |
| 版本与未知值 | 通过 | 只接受 `0.1.x`；wire enum 全闭合；未知上游标签只生成诊断/协议错误，不猜语义 |
| Codex 真实映射 | 通过 | ADR-0012 仅适用于 Codex；无虚构 `turn/failed`，失败从真实 `turn/completed.status/error` 归约 |
| Kotlin UniFFI 生成与编译 | 通过 | §3 机器输出，842 个 class |
| Swift UniFFI 生成 | 通过 | 两个 Swift 源文件、两个 FFI header 与 modulemap 已生成 |
| Swift UniFFI 编译 | **阻塞（UB-R1-S）** | 当前环境 `swiftc` / `swift` / `xcrun` 均不存在；WSL Ubuntu 也无 Swift。按 [ADR-0013](../adr/0013-platform-track-order.md) 携带至 R8，解除路径见 [T-102](../tasks/T-102.md) |
| Rust workspace 基线 | 通过 | §4 全部门禁绿色；主管已独立复跑，见 §0.1 |
| T-100 草稿 | 通过 | 范围是单一 Codex 纵切，含三 fixture、approval join、decline、summary、未知/类型错/join fail/replay。**已由主管改为 active 并加入两阶段交付** |
| R1 总结论 | **有条件通过（携带 UB-R1-S）** | 见 §0.5。Swift 编译仍是硬门禁，只是改为 R8 前置，不以生成成功代替 |

## 2. 协议与 `kaleido-proto`

已完成模块：`ids`、`content`、`host`、`capability`、`session`、`turn`、`queue`、
`attention`、`workflow`、`error`、`command`、`effect`、`projection`。

合同修正覆盖主管给出的二十项结论：

1. R-P1 包含实际使用的 `u8` / `u32` 等标量；
2. 所有 wire enum 与辅助 record 完整定义；
3. 八个 projection 与 `ProjectionPayload` 逐字段定义且字段同名；
4. Host/Project/Session/Workflow 四类流分别有同型 snapshot；
5. 逻辑 Project 支持多 runtime binding，Step 指定 binding + worktree；
6. workflow 覆盖 advance/retry/rework/skip/cancel/reassign 与闭合转移表；
7. §4.10 定义手机读取正文的数据通路；
8. 上游原始 ID 由 Broker opaque handle 隔离；
9. reason/note/blocker 只用枚举或 Sensitive `ContentRef`；
10. `ErrorCode::UpstreamBlocked` 强制携带 `BlockerId`；
11. attention 回复与 workflow gate 可验证、可回答；
12. queue reorder 必须恰好等于目标 session 的全部 Pending 集合；
13. cursor 使用 checked overflow，不饱和或回绕；
14. `CommandAck` 不复制 cursor；
15. pre-1.0 只兼容 `0.1.x`；
16. UACP wire enum 全闭合，未知上游值只进诊断；
17. 删除不存在的 Codex `turn/failed` 映射；
18. ADR-0012 明确只授权 Codex；
19. `AGENTS.md` §3.2 同步为 Codex pointer decoder，OpenCode/Claude 各自评估；
20. 文档与 Rust wire 类型双向审计：135 个 serde wire 类型全部能在协议中找到，
    无 `root_ref/root`、`note_ref/note`、`path_ref/path` 等漂移。

`kaleido-proto` 不依赖 tokio、IO、provider SDK 或未定型 JSON。36 条契约测试覆盖用户
要求的 15 类语义；JSON round-trip 覆盖 16/16 `StateEffect`、18/18 `Command`、
8/8 `ProjectionPayload`、4/4 `SnapshotPayload`。

三份真实 Codex fixture 均由测试直接读取：

- `01-simple-turn.jsonl`
- `03-permission-approve.jsonl`
- `04-permission-deny.jsonl`

## 3. UniFFI 双端探针

版本与依赖：

- Rust / Cargo `1.94.0`
- UniFFI `=0.32.0`
- Gradle `8.14`
- Kotlin JVM plugin `2.2.20`
- JNA `5.19.1`
- Java / Javac `22.0.2`

`kaleido-core::binding_probe` 的签名直接使用 `CommandEnvelope`、
`ProjectionEnvelope`、`Option<CanonicalError>`、`Vec<StateEffect>`，因此生成器与
语言编译器检查的是真实 canonical record、具名字段 enum、Option、Vec、嵌套命令/
错误/投影图，不存在第二套 DTO。

生成输出：

```text
Code generation complete, formatting with ktlint (use --no-format to disable)
Code generation complete, formatting with ktlint (use --no-format to disable)
target/uniffi/kotlin/.../kaleido_core.kt    43690 bytes
target/uniffi/kotlin/.../kaleido_proto.kt 330837 bytes
target/uniffi/swift/kaleido_core.swift      22538 bytes
target/uniffi/swift/kaleido_proto.swift    357744 bytes
```

本机没有 `ktlint` / `swift-format`，因此生成器打印了非致命格式化 warning；没有
unsupported 或 skipped 类型。生成物位于被忽略的 `target/`，不提交。

Kotlin 实际编译：

```text
> Task :compileKotlin

BUILD SUCCESSFUL in 30s
2 actionable tasks: 2 executed
KOTLIN_PROBE_EXIT=0
KOTLIN_CLASS_COUNT=842
```

Swift 阻塞：

```text
swiftc=NOT_FOUND
swift=NOT_FOUND
xcrun=NOT_FOUND
WSL Ubuntu-24.04: command -v swiftc / swift 均无输出
Docker Desktop Linux engine: not running
```

🛑 R1 Swift UniFFI 编译阻塞

问题：当前 Windows 与已有 WSL 环境均没有 Swift 编译器；Docker engine 也不可用。

影响：Swift 源码虽然成功生成，但 R1 要求的是“生成并编译通过”，不能标记通过。

已完成部分：真实 canonical 类型的 Rust/UniFFI 编译、Kotlin 生成与编译、Swift 生成。

等待条件：在受支持且已有 Swift 工具链的 macOS/Linux 环境编译
`target/uniffi/swift/{kaleido_proto.swift,kaleido_core.swift}` 与
`crates/kaleido-core/bindings/swift-probe/Probe.swift`。本次未静默安装系统工具。

## 4. Workspace 与文档验证

恢复预跑的真实摘要：

```text
cargo fmt --all -- --check
# exit 0, no output

cargo xtask check-deps
<== check-deps: ok; 5 workspace member(s), 1 internal edge(s), 2 crates/* manifest(s)

cargo xtask lint-forbidden
lint-forbidden: A-2 agent-name-branch exemptions=0
lint-forbidden: A-11 version-branch exemptions=1
<== lint-forbidden: ok

cargo clippy --all-targets -- -D warnings
Finished `dev` profile ... exit 0

cargo test -p kaleido-proto
test result: ok. 36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

cargo test --workspace
test result: ok
# recorder 164/164；xtask dependency tests 14/14；proto 36/36；其余 suites 全绿

cargo xtask fixtures verify
<== fixtures-verify: ok; 5 file(s), 220 record(s)
  (codex: 3, acp-claude: 1, opencode: 1)

cargo xtask ci
<== test: ok
<== fixtures-verify: ok; 5 file(s), 220 record(s)
  (codex: 3, acp-claude: 1, opencode: 1)

git diff --check
# exit 0（只有 Git 的 LF→CRLF 工作区提示）

文档相对链接检查
MARKDOWN_FILES=50
BROKEN_RELATIVE_LINKS=0
```

历史 recorder 测试的授权窄修复仍断言外部目标被拒绝、结构化诊断存在，且原始绝对路径、
用户名和真实 home 不出现；只接受文档批准的 `<HOME>` / `<OUTSIDE_PATH>` 确定性
占位符。生产脱敏顺序、安全守卫和 fixture 均未修改。

## 5. 变异验证

以下改坏均实际写入生产实现、运行精确测试得到红色，再立即恢复。最终 36 条 proto 测试
全绿，工作区不含变异代码。

```text
1. Declined 被错误算作 failure
test declined_item_is_terminal_but_not_a_failure_and_turn_can_complete ... FAILED
left condition: ItemStatus::Declined.is_failure() == true
assertion failed: !ItemStatus::Declined.is_failure()

2. HandshakeDeclared 也被当作 steer delivered 证明
test steer_stays_queued_without_runtime_observation_or_capability ... FAILED
left: Ok(())
right: Err(UnprovenSteerDelivery { evidence_source: HandshakeDeclared })

3. HistoryRead 被错误提升成 LiveObserve
test history_capabilities_cannot_be_promoted_to_live_binding ... FAILED
left: Ok(())
right: Err(LiveBindingUnsupported { missing: "live_observe" })

4a. cursor gap 被忽略
test cursor_sequence_rejects_repeat_gap_cross_stream_and_overflow ... FAILED
left: Ok(())
right: Err(CursorGap { expected: 2, found: 3 })

4b. cursor overflow 被饱和为 u64::MAX
test cursor_sequence_rejects_repeat_gap_cross_stream_and_overflow ... FAILED
left: Ok(Cursor { seq: 18446744073709551615 })
right: Err(CursorOverflow)

5. raw upstream ID 被当作合法 binding handle
test raw_upstream_ids_are_rejected_in_logs_and_projections ... FAILED
left: Ok(())
right: Err(InvalidProviderBindingId)
```

## 6. 新增依赖及理由

| 依赖 | 位置 | 理由 |
|---|---|---|
| `uniffi = "=0.32.0"` | workspace；proto optional feature；core + CLI | 在真实 canonical 类型上派生绑定元数据并生成 Kotlin/Swift；精确钉定避免两端生成器版本漂移 |
| Kotlin JVM plugin `2.2.20` | Kotlin probe | 编译生成的 Kotlin binding 与消费 probe；只属于探针构建 |
| JNA `5.19.1` | Kotlin probe | UniFFI 生成的 JVM FFI 层直接依赖 JNA |

没有加入 tokio、IO、provider SDK 或未定型 JSON 到 `kaleido-proto`。

## 7. 偏离、发现的问题与未完成项

- 偏离：Swift 只生成、未编译；这是硬阻塞，已如实保留。
- 未完成项：仅 Swift 编译证据。没有把它延期成活动 T-101，也没有让 T-100 越过 R1。
- 发现并修复：依赖规则已含 `kaleido-state`，但 xtask 自测仍断言旧十 crate 矩阵；
  自测现同步为十一 crate，并继续校验 `kaleido-core` 的完整 allow-list。
- 发现并修复：`ContentReadChunk.next_offset` 原先只在文档要求 checked；现在 size、
  overflow、eof/continuation 一致性均有 validator 与错误路径测试。
- 发现并修复：合法形状的 binding handle 原可跨 Session/Turn/Item/Interaction/Ack
  串用；现在 kind 与实体严格绑定并有错误路径测试。
- 未修改任何 schema 或 fixture；未创建 adapter/reducer/hostd/mobile/transport/relay
  产品逻辑；未恢复旧 M1、T-001～T-014；未实现 T-100；未提交、未推送。

## 8. 任务状态

- **R1：未通过。**
- **T-100：blocked by R1。**
- 当前没有活动产品任务。

---

<details>
<summary>恢复前旧评审草案（已撤销，仅保留历史，不作为当前结论）</summary>

以下内容是本次一次性恢复前的旧草案，包含当时的 47-test、条件通过与 T-101 方案；
均被上面的当前评审取代。

---

## 1. 逐条对照 MILESTONES R1 门禁

| 门禁 | 结论 | 证据 |
|---|---|---|
| 合同能表达项目索引、历史、活动会话、队列、Attention Inbox、工作流 | 通过 | [PROTOCOL](../PROTOCOL.md) §8 八个投影；§4.1 `Project`/`SessionCounts`；§4.3 `HistorySource`；§4.6 `QueueEntry`；§4.7 `AttentionItem`；§4.8 `Workflow`/`Step`/`Artifact` |
| 明确 queue 与 steer | 通过 | §4.6 R-P9 + §6 故意不设 steer 命令；`QueueState::DeliveredAsSteer` 强制要求 `SteerAcknowledgement.source == ObservedInTraffic`；测试 `delivered_steer_requires_observed_runtime_proof` |
| 明确 history 与 live | 通过 | §4.3 R-P7 双字段 + `LiveBinding::validate_against`；测试 `history_capability_alone_does_not_permit_an_observing_binding` |
| 明确 decline 与 error | 通过 | §4.5 / §7 R-P8；`ErrorCode` 中**不存在**拒绝码；测试 `declined_item_is_terminal_but_not_a_failure`、`a_decline_does_not_change_the_turn_outcome` |
| 用真实 fixture 验证至少两个 reducer 难点 | 通过（验证了三个） | 见下 §3 |
| 移动端绑定对最小真实类型编译通过 | **未取得机器证据** | 见 §4 与 UB-R1-1 |

MILESTONES R1 的「禁止」项：本轮没有恢复 T-014，也没有在 proto 之外创建
adapter 自有的临时全局模型。

---

## 2. 交付物自检

| 命令 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| `cargo xtask check-deps` | 通过（4 个 workspace member，0 条内部边，1 份 `crates/*` manifest） |
| `cargo xtask lint-forbidden` | 通过（A-2 豁免 0，A-11 豁免 0；A-4 对 `kaleido-proto` 无命中） |
| `cargo clippy --all-targets -p kaleido-proto -- -D warnings` | 通过 |
| `cargo test -p kaleido-proto` | **47 passed / 0 failed** |
| `cargo xtask fixtures verify` | 通过（5 个文件，220 条记录；codex 3 / acp-claude 1 / opencode 1） |

### 2.1 测试真实性抽检

按 `AGENTS.md` §2.3，主管自己做了两次「改坏实现 → 测试变红」验证：

```text
# 1) 把 ItemStatus::is_failure 改成也包含 Declined
test declined_item_is_terminal_but_not_a_failure ... FAILED
test result: FAILED. 46 passed; 1 failed

# 2) 把 QueueEntry::validate 里的 steer 证据检查改成 if false
test delivered_steer_requires_observed_runtime_proof ... FAILED
test result: FAILED. 46 passed; 1 failed
```

两处均已还原，当前 47 passed。

### 2.2 A-4 门禁的实际约束

`xtask` 的 A-4 扫描器检查 `kaleido-proto` 的**全部字符串字面量，包括文档注释**
（`///` 在 syn 中就是 `#[doc = "..."]` 的 `LitStr`）。因此 proto 里连注释都不能写
上游 discriminator。这条已经在本轮实际生效，不是纸面规则。

---

## 3. 用真实 fixture 验证的三个 reducer 难点

全部来自 2026-07-30 录制的 Codex fixture，不是 schema 推断：

1. **审批 join** —— `tests/fixtures/codex/03-permission-approve.jsonl:50` 的审批请求
   params 只有 `threadId`/`turnId`/`itemId`/`startedAtMs`/`reason`/`grantRoot`，
   可展示上下文在 `:48`。据此把 `JoinState` 定为 `ApprovalRequest` 的必填字段，
   并要求 `Unjoined { ItemNotYetSeen }` 可渲染。
2. **decline 是 Item 终态** —— `04-permission-deny.jsonl:50` 回
   `{"decision":"decline"}`；`:53` item `status: "declined"`；`:84` turn 仍
   `turn/completed`。据此定 R-P8，并在 `ErrorCode` 里刻意不设拒绝码。
3. **turn 结束报文不是 transcript 来源** —— `03-permission-approve.jsonl:83` 的
   `turn/completed` 带 `itemsView: "summary"`，`items` 只有最后一条 agentMessage，
   而该 turn 实际有 6 条 item。据此在 §4.4 规定 `Turn.item_ids` 必须由逐条 item
   累积，并在 `Turn::validate` 里加了重复引用检查。

这三条都写进了 [PROTOCOL.md](../PROTOCOL.md) §11.2，带行号，可复查。

---

## 4. UniFFI 可表达性：做了什么、没做什么

**做了**：把可表达性写成协议规则 R-P1，并对 `kaleido-proto` 全量人工审计。
当前 proto 只使用：具名字段 record、无字段或具名字段 enum、`String`、`bool`、
`i64`、`u64`、`Vec<T>`、`Option<T>`。全库确认**不存在**泛型、元组、元组结构体
（ID 类型刻意写成 `{ value: String }`）、trait object、`serde_json::Value`、
map 类型、`u128`、`f64`、`Duration`、`SystemTime`。

**没做**：没有运行任何 bindgen。因此不能声称「Swift/Kotlin 绑定编译通过」。

### 🛑 UB-R1-1 登记

```text
🛑 MILESTONES R1「移动端绑定对最小真实类型编译通过」阻塞

目标：用机器证据证明 canonical 类型可被 UniFFI 表达。
公开路径已验证：无（本轮未运行 bindgen）。
缺失的协议能力：不适用；这是本项目自身的验证缺口，不是上游缺口。
不能采用的伪实现：把人工审计写成「绑定编译通过」；或为了让生成器过而擅改 proto。
对最终需求的影响：不影响 T-100（纯 Rust 纵切）。会影响 R3 Android 纵切开工。
可继续推进的独立纵切：T-100 全部内容。
复查触发条件：T-101 交付。
```

---

## 5. 本轮实施中做出的三处协议调整

写代码时发现了三处纸面设计缺陷，已同步改到 [PROTOCOL.md](../PROTOCOL.md)：

1. **枚举内部标签占用 `kind`**。带数据的枚举统一用 `#[serde(tag = "kind")]`，
   于是变体内不能再有名为 `kind` 的字段。`CompletionCondition::ArtifactProduced`
   的字段改为 `artifact_kind`，规则写进 §1。
2. **`CanonicalError.message` 由 `ContentRef` 改为 `summary: String`（≤ 256 字节）
   加 `detail: Option<ContentRef>`**。原设计把所有错误文本都变成需要二次取正文的引用，
   反而让「可入日志的那部分」无处安放；新形状让 §10 的日志白名单在类型层面成立。
   附带效果是消除了 `clippy::large_enum_variant`。
3. **快照移出订阅响应**。原 `SubscribeResult::Snapshot { snapshot }` 让一个控制响应
   携带可能达数 MB 的快照。改为 `SubscribeAck { stream, outcome }`，
   `SnapshotFollows { snapshot_cursor }` 之后快照作为独立消息送达。

三处都属于「实现暴露设计缺陷 → 先改协议再改代码」，不是实现方擅自变更。

---

## 6. 新登记的工程缺陷（不阻塞 T-100）

| ID | 缺陷 | 处置 |
|---|---|---|
| D-R1-1 | `cargo xtask check-deps` 的自建 TOML 读取器不支持点号键，导致 `edition.workspace = true` 被拒。`spikes/*` 因豁免未暴露该问题 | 暂用 `edition = { workspace = true }` 绕过并在 `crates/kaleido-proto/Cargo.toml` 与 T-100 §3 写明。修 xtask 单独开卡，不夹带进 T-100 |
| D-R1-2 | `kaleido-recorder` 的 `outside_permission_target_is_rejected_with_redacted_structured_diagnostics` 先于本轮失败：临时目录落在家目录时先被脱敏成 `<HOME>`，旧断言要求 `<OUTSIDE_PATH>`。因此 `cargo xtask ci` 会在第 5 步停住，第 6 步 fixtures verify 不会执行 | 冻结区，本轮不修。T-100 的 DoD 改为逐门禁单独执行，并要求原样粘贴 `cargo test --workspace` 输出、证明失败集合未增加 |

---

## 7. ADR-0012 为什么必要

[ADR-0005](../adr/0005-schema-normalization-layer.md) 已实测记录 typify 无法消化 Codex 完整 schema、
progenitor 不支持 OpenAPI 3.1，并在「影响的门禁」里预留了出口：
若某家只能人工维护类型子集，必须另开 ADR 记录妥协与漂移检测手段。

[ADR-0012](../adr/0012-provider-decode-strategy.md) 行使该出口，但没有退回手写上游类型：它规定**不产生**上游类型，
改为「钉定 JSON Pointer 表 + `required-surface.toml` 归属 + 快照可解析性测试 +
未知/变形报文显式失败」。这样 R-4 的漂移风险仍有机器守卫，同时把
「先生成完三家类型」从产品开工前置里彻底移除——那是 M1 停滞的直接成因之一。

---

## 8. R1 之后的状态

- 活动任务卡：**[T-100](../tasks/T-100.md)**（唯一）。prompt 见 [T-100-codex-prompt.md](../tasks/T-100-codex-prompt.md)。
- 排队任务卡：[T-101](../tasks/T-101.md)（UniFFI 探针），T-100 审核通过后下发。
- 仍然开放的上游阻塞：Codex Desktop 与 Claude Code 原生 GUI 的第三方实时附着
  （[REQUIREMENTS](../REQUIREMENTS.md) §8 两格），归 R7，不影响 T-100。
- `docs/PROTOCOL.md` 与 `crates/kaleido-proto` 从现在起是合同。任何修改必须先走 ADR。

</details>
