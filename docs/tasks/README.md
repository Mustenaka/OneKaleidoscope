# 任务卡状态

> 生效：2026-07-30（R1 主管评审后更新）

## 活动

**当前活动：仓库收敛**（prompt：[repo-convergence-prompt.md](repo-convergence-prompt.md)）。

本地 `main` 与 `origin/codex/t-102-uniffi-probe` 两个来源都不完整。主管已逐文件比对：
分支是 `ae9da23` 的直系后代，可 `--ff-only` 快进，**无合并冲突**；
差异恰好是 `.gitattributes`（分支侧）与 12 个主管文档（主树侧）。
见 [T-102 门禁结果](../gates/T-102-result.md) §4。

## 排队（R3 硬前置，收敛后依次下发）

| 卡 | 范围 | 状态 |
|---|---|---|
| [T-103](T-103.md) | **P-1**：让 `AttentionState::Answered` 能表达「观察到的外部应答」 | queued。授权改 proto，**先写 ADR 等批准** |
| [T-104](T-104.md) | **D-B1**：让 `LiveControl` 可达，否则手机永远只读 | queued，T-103 通过后下发。主体是回答语义问题 |

prompt：[T-103-codex-prompt.md](T-103-codex-prompt.md)、[T-104-codex-prompt.md](T-104-codex-prompt.md)

两张卡**必须分开下发**，不得合并——它们是不同的问题，合并会让审核失焦。
两张结清后才写 R3 的卡。

## 已完成

| 卡 | 范围 | 结果 |
|---|---|---|
| [T-100](T-100.md) | Codex app-server → 钉定 decoder → reducer → canonical state → durable log → Rust diagnostic client | **通过，2026-07-31**。[门禁结果](../gates/T-100-result.md)、[阶段 A 评审](../gates/T-100-stage-a-review.md)。R2 达成 |
| [T-102](T-102.md) | macOS CI 真实 Swift 编译 + UniFFI callback / object / async / throwing 四面探针 | **通过，2026-08-02**。[门禁结果](../gates/T-102-result.md)、[证据](../gates/T-102-evidence.md)。UB-R1-S 与 G-R1-1 解除。四次阻塞裁定见 `T-102-unblock-reply*.md` |

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
| **D-B4** | `crates/kaleido-adapter-codex/src/platform/mod.rs` 只枚举三个 `target_os`、无缺省分支，在第四种目标上不编译（`android` ≠ `linux`） | **已授权在 [T-102](T-102.md) §5.4 修复** |
| **D-B5** | CI 从未在 macOS/Ubuntu 真跑过，冻结区积累了跨平台 lint 缺陷 | 首处已授权在 [T-102](T-102.md) §5.4 修复；本机可用 `--target aarch64-linux-android` 提前扫纯 Rust 部分 |
| **D-B6** | 跨平台路径校验必须显式决定 `\` 在 Unix 上算普通字符还是分隔符（安全校验器倾向 fail-closed） | **R9 前置**。写文件树/预览/Git 的路径校验前先定规则，**不得照抄 recorder**。见 [ADR-0015](../adr/0015-frozen-spike-tests-are-windows-only.md) D-4 |
| **D-B7** | macOS `/var` 是系统符号链接别名，「祖先是链接即不安全」的校验必然误判 | R9 前置，同上 |
| **D-B8** | 脱敏占位符优先级：`<HOME>` 先于 `<SANDBOX>` 命中。**不是泄漏**，是标签精度 | 随 recorder 退出 CI（[ADR-0016](../adr/0016-recorder-out-of-ci-and-single-backslash-roots.md) D-1），**仍未修复**；R4 脱敏定稿时复查 |
| **D-B9** | `xtask` 泄漏扫描器不识别单前导反斜杠根（`\tmp\foo`），Windows 上是合法绝对路径形态，含它的 fixture 会绕过扫描 | **已授权在 [T-102](T-102.md) §5.6 修复**（fail-closed 方向） |
| **D-B10** | `cargo xtask schema diff` 对**工作树字节**算 git blob 哈希；无 `.gitattributes` + `core.autocrlf=true` 时，干净 Windows clone 会**假报 schema 漂移**，削弱 ADR-0012 D-2 的守卫 | 由 [ADR-0017](../adr/0017-line-ending-determinism.md) D-1 消除，已在干净 Windows clone 上复跑 |
| **D-B11** | `schema diff` 报 1 处 out-of-surface removal（required surface 仍兼容，exit 0）；本机 OpenCode `1.18.9` 而快照 `1.18.8` | **R5 前置**。接入 OpenCode 前先对齐快照与实机版本并复跑 `schema diff`，不得带着版本错配写 adapter |
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
