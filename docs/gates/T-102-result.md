# T-102 门禁结果

- 日期：2026-08-02
- 实现方：Codex
- 评审：项目主管（Claude Code, Orchestrator）
- 结论：**通过。UB-R1-S 与 G-R1-1 解除。**
- 实现方证据：[T-102-evidence.md](T-102-evidence.md)

T-102 有四次阻塞，全部由实现方主动上报、无一次用 `allow` / `#[ignore]` / 调整执行顺序
绕过。四次的根因同源：**仓库此前从未推送，本地工作树的偶然状态被当成了代码的真实前提。**

---

## 1. 主管独立复验

### 1.1 我能在本地验证的

| 检查 | 结果 |
|---|---|
| `.gitattributes` 内容是否与 [ADR-0017](../adr/0017-line-ending-determinism.md) D-1 一致 | **一致**，逐字相同（`* text=auto eol=lf` + `schemas/**` / `tests/fixtures/**` 的 `-text`） |
| `xtask/tests/deps.rs` 是否 fail-loud | **是**。`assert_ne!(rules, normalized_rules, "test setup must remove the shared adapter allow-list entry")` —— 正是 ADR-0017 D-2 要求的形态，不只是规范化 CRLF |
| 分支相对 `6de6eb0` 的改动范围 | **恰好 3 个文件**：`.gitattributes`、`docs/gates/T-102-evidence.md`、`xtask/tests/deps.rs` |
| 四类调用面是否真的被消费（不是只 import） | **是**。Swift 侧 `ProjectionProbeSink: ProjectionProbeCallback` 实现两个方法；`ProjectionSubscriptionProbe` 构造→subscribe→unsubscribe；`catch let BindingProbeError.Canonical(error:)`；`await asyncBindingProbe(ack:)` |
| 主工作树 `cargo xtask ci` | exit 0 |

### 1.2 主管自己的变异（实现方**未报告**的位置）

实现方自报两处（Swift 重名导出 → CI 红；fail-loud setup → `assert_ne!` 触发）。
主管另选一处：把 `ProjectionSubscriptionProbe::unsubscribe` 改成**不释放** callback。

```text
test tests::subscription_calls_and_retains_the_foreign_callback_until_unsubscribe ... FAILED
panicked at crates\kaleido-core\src\lib.rs:240:9:
assertion failed: !has_callback(&probe)
test result: FAILED. 3 passed; 1 failed
```

已还原，`cargo test -p kaleido-core` 恢复 4 passed。**订阅句柄的生命周期有测试守住**——
这对 R3 很重要：手机订阅之后必须能真的退订，否则回调会一直持有外语言对象。

### 1.3 §5.4 的未闭合项，主管在本地补上了

实现方诚实声明：首轮跨平台修复前后的 per-binary passed-count 原始日志未保留，
且回溯 checkout 会先撞上 D-B8，无法事后重造。**没有把它伪装成已证明，这是对的。**

主管改用另一条证据链闭合它——直接对照 R1 记录的基线：

```text
cargo test -p kaleido-recorder
test result: ok. 164 passed; 0 failed        ← 与 R1 评审记录的 164 逐字一致
（其余 binary 全绿，本机合计 270 passed / 0 failed）
```

结构性论证同向：给 `use` 语句加 `#[cfg(windows)]` 在 Windows 上不改变任何东西
（`cfg(windows)` 为真，导入保留），在非 Windows 上只移除一个未使用导入；
`unused_mut` 与 adapter 的第四目标缺省分支同理。**这类改动在结构上不可能减少测试。**

判定：**该格闭合**，依据是「与 R1 基线逐字一致」而非「前后日志对比」。
证据形式与原要求不同，已如实记录。

### 1.4 我**不能**在本地验证的

本机没有 `gh`，因此**主管没有独立核验 GitHub CI 的运行结果**。
下列结论来自实现方提供的 run URL，由负责人自行点击复核：

| 用途 | commit | run |
|---|---|---|
| 故意改坏 Swift 导出 → 变红 | `d971b238` | [30614223755](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30614223755) |
| 三平台全绿 | `4d40c76` | [30707060011](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30707060011) |
| 最终 tip 三平台全绿 | `31c8a2d` | [30708454762](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/30708454762) |

变异的原始错误 `invalid redeclaration of 'probeProtocolVersion()'` 与所改动的位置一致，
可信度高；但**这一格是「实现方提供的证据」而不是「主管复验的证据」**，如实标注。

---

## 2. DoD 判定

| 格 | 结论 |
|---|---|
| §5.1 Swift 编译证据（UB-R1-S） | 通过（CI 证据来自实现方，见 §1.4） |
| §5.1 门禁有效性：改坏 → CI 变红 | 通过。这是本卡最关键的一格 |
| §5.2 四类调用面两端消费（G-R1-1） | 通过，主管独立核验了 Swift 侧的实际调用 |
| §5.3 边界与工程门禁 | 通过。`crates/kaleido-proto/**`、`PROTOCOL.md`、`docs/adr/**`、`schemas/**`、`tests/fixtures/**` 在 `b11b32c..31c8a2d` 区间零改动 |
| §5.4～§5.7 四次授权的实现 | 通过，逐条核验见 §1.1 |
| §5.4 前后测试计数原始日志 | **以替代证据闭合**，见 §1.3 |

### G-R1-1 的结论，以及它**没有**回答的

> **R3 的投影推送能走 UniFFI 回调。**

依据：Kotlin 与 Swift 两端都实现并编译了 `ProjectionProbeCallback`，Rust 通过
`ProjectionSubscriptionProbe.subscribe` 调用它；同一门禁还编译了 object、
携带 `CanonicalError` 的失败载荷、以及返回 `CommandAck` 的 async 面。

实现方明确写出了这个结论的边界，主管确认这个边界是对的：
**只证明了绑定形状能表达、能生成、能编译。没有证明**回调的线程调度、背压、
进程被杀后的恢复，也没有生产订阅生命周期。这些是 R3 自己要解决的问题，
不能引用本卡当作已解决。

---

## 3. 新登记

| ID | 内容 | 处置 |
|---|---|---|
| **D-B11** | `cargo xtask schema diff` 报告 1 处 out-of-surface removal（0 added / 0 changed / 1 removed），exit 0、required surface 仍兼容。同时本机 OpenCode 为 `1.18.9` 而快照为 `1.18.8` | 实现方**正确地没有断言因果**。R5（OpenCode 接入）开工前必须先把快照与实机版本对齐并复跑 `schema diff`，不得带着版本错配写 adapter |

D-B6 / D-B7 / D-B8 / D-B10 状态不变，**仍未修复**，不得写成已解决。

---

## 4. 仓库状态：**这是当前最大的风险，不是技术问题**

两个来源都不完整，必须由负责人收敛：

| 来源 | 内容 | 缺什么 |
|---|---|---|
| 本地 `main` 工作树（停在 `ae9da23`，37+ 未提交路径） | R1、R2 的四个 crate、主管在 T-102 期间写的 ADR-0015/0016/0017、T-102 §5.4～§5.7 授权正文、D-B6～D-B11 登记、四份 unblock 裁定 | `.gitattributes`、`deps.rs` 的 fail-loud 修复 |
| `origin/codex/t-102-uniffi-probe`（`31c8a2d`） | T-102 的全部实现与 CI 证据 | 主管的全部文档 |

实现方**正确地没有**自行合并或覆盖主工作树。收敛动作属于负责人。

在收敛完成前：`docs/gates/T-102-evidence.md` 在主工作树里是 untracked 文件，
内容与分支 blob 一致；主工作树的 `xtask/tests/deps.rs` 仍是**未修复**的旧版本，
所以主工作树上的 `cargo xtask ci` 绿是在旧 CRLF 前提下的绿。

---

## 5. 判定与下一步

**T-102 通过。UB-R1-S 与 G-R1-1 解除。**

R3 剩余两条硬前置，都要改 `kaleido-proto` 或协议，**各开一张卡，不得合并**：

1. **P-1** —— `AttentionState::Answered` 无法表达「观察到的外部应答」；
2. **D-B1** —— `LiveControl` 结构性不可达，`LiveBinding::Controlling` 永远到不了，
   手机会一直渲染成只读。

两张卡都必须先写 ADR 再改代码。R3 的卡在这两张结清之后才写。

在开卡之前，建议负责人先完成 §4 的仓库收敛——四次阻塞的共同根因就是本地状态与
已推送状态脱节，带着两个不完整的来源开下一张卡会让同样的问题继续复利。
