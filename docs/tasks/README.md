# 任务卡状态

> 生效：2026-07-30（R1 主管评审后更新）

## 活动

| 卡 | 范围 | 状态 |
|---|---|---|
| [T-102](T-102.md) | 解除 UB-R1-S（macOS CI 上的真实 Swift 编译）+ 把 UniFFI 探针扩展到 callback / object / async / throwing 面 | **active**，2026-07-31 下发。**必须在 R3 开工前完成** |

prompt：[T-102-codex-prompt.md](T-102-codex-prompt.md)

## 已完成

| 卡 | 范围 | 结果 |
|---|---|---|
| [T-100](T-100.md) | Codex app-server → 钉定 decoder → reducer → canonical state → durable log → Rust diagnostic client | **通过，2026-07-31**。[门禁结果](../gates/T-100-result.md)、[阶段 A 评审](../gates/T-100-stage-a-review.md)。R2 达成 |

prompt 存档：[T-100-codex-prompt.md](T-100-codex-prompt.md)（阶段 A + 带上下文的阶段 B 附录）、
[T-100-stage-b-codex-prompt.md](T-100-stage-b-codex-prompt.md)（阶段 B 冷启动版）

## R3 开工前必须先开的卡

P-1 与 D-B1 都要改 `kaleido-proto` 或协议，必须由主管单独开卡授权。见下表。

## 已登记但尚未开卡

| ID | 内容 | 处置 |
|---|---|---|
| **P-1** | `AttentionState::Answered` 强制 `command_id`，但被观察到的外部应答没有本地命令 | **R3 硬前置**。要改 `kaleido-proto`，必须单独开卡授权。见 [ADR-0014](../adr/0014-codex-approval-families-and-timestamp-units.md) D-3 |
| **D-B1** | `LiveControl` 无任何代码路径可证明，`LiveBinding::Controlling` 结构性不可达 | **R3 硬前置**。手机不解决就永远只读。见 [T-100 门禁结果](../gates/T-100-result.md) §4 |
| D-B2 | 活进程树终止只测了「已退出」分支，杀活树无断言 | R4 前置（hostd 变常驻服务时孤儿才开始积累） |
| D-B3 | 跨 stream 投影的 cursor 语义待确认 | 不阻塞 |
| P-2 | Codex 审批不带过期时间 | 上游事实，移动端需能渲染「无过期时间」的审批 |
| D-R1-1 | `cargo xtask check-deps` 的自建 TOML 读取器不支持点号键 | 不阻塞。当前用 `edition = { workspace = true }` 绕过 |

## 冻结

- T-001～T-013：冻结，仅保留历史、实录步骤和研究证据。
- T-014：在实现前撤销。它先于 `PROTOCOL.md` 定义临时全局状态，不符合新基线。
- T-101：其 UniFFI 生成/Kotlin 编译工作已并入一次性 R1 恢复；剩余部分由 [T-102](T-102.md) 取代。
- `M1-queue.md`：已删除，禁止从 Git 历史恢复后继续下发。

任何冻结任务中的「待实现」「下一步」「前置」「DoD」都不再生效；与当前文档冲突时以
[`docs/STATUS.md`](../STATUS.md) 的优先级为准。

冻结保留而非删除任务卡，是为了保存失败过程与实测依据，避免未来重复踩坑。
