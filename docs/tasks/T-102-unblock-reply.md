# T-102 阻塞裁定 — 回给 Codex 的 prompt（原样粘贴）

> 2026-07-31，主管批复实现方的 `spikes/**` 边界例外请求。
> 粘贴横线以内的全部内容。

---

## 裁定：批准，但边界比你申请的宽一点，也比你想的更需要一次做完

你的阻塞报告处理得对。你**没有**用 `allow`、跳过 clippy 或调整执行顺序绕过，而是停下来
请示——这正是 `AGENTS.md` §5 要的。诊断也核实无误：我读了
`spikes/kaleido-recorder/src/agents/mod.rs`，`validate_exact_permission_cwd` 的唯一使用点
在同文件 `:605`，位于 `#[cfg(windows)]` 测试内，而 `:407` 的导入没有条件。
**这是既有缺陷，不是你引入的。**

补充一条背景你可能不知道：这个仓库此前**从未推送过**，所以 macOS / Ubuntu CI 是第一次
真正运行。冻结区里大概率不止这一处。

## 授权范围（写进了 `docs/tasks/T-102.md` §5.4，以那里为准）

### 允许

在 `spikes/**` 与 `xtask/**` 中，**仅**为让代码在非 Windows 平台通过编译与 lint 而做的
**条件编译属性修正**。你申请的那一行照批：

```rust
#[cfg(windows)]
use super::validate_exact_permission_cwd;
```

### 禁止

改测试断言 / 期望值 / 被测语义；删除或 `#[ignore]` / `#[allow]` 掉任何测试或 lint；
动生产脱敏逻辑、fixture、schema；借这个口子做任何与「让它在非 Windows 上编译」无关的
整理或重构。`crates/kaleido-proto/**`、`docs/PROTOCOL.md`、`docs/adr/**`、
`schemas/**`、`tests/fixtures/**` 仍然是硬冻结。

### 交付要求

1. **逐条列出**每一处改动：文件、行号、改前、改后，以及「唯一使用点在哪里、
   为什么这个 cfg 与它一致」；
2. 冻结区哈希基线**重新建立**，并说明差异恰好等于你列出的条目，不多不少；
3. 证明语义未变：改动前后 Windows 上 `cargo xtask ci` 都 exit 0，
   且测试**数量与通过数不变**。

## 请先把它们一次找齐，不要一次 CI 修一个

我实测：本机已装 `aarch64-linux-android` 目标，它的 `cfg(windows)` 为假，所以能在**本地**
复现 macOS / Ubuntu 的同类失败，不用等 CI：

```bash
cargo clippy --all-targets --target aarch64-linux-android -p kaleido-core -p kaleido-proto -p kaleido-state -p kaleido-adapter -p kaleido-adapter-codex -p kaleido-hostd
```

注意 `spikes/kaleido-recorder` 与 `xtask` 间接依赖 `ring` / `aws-lc-sys` / `blake3`，
需要 C 交叉工具链，本机跑不通——那两个包只能靠 CI 验证。但纯 Rust 部分先在本地扫干净，
能省掉好几轮 CI 往返。

## 顺带：我用上面这个方法发现了另一处，你还不知道，一并授权你修

`crates/kaleido-adapter-codex/src/platform/mod.rs` 只枚举了 linux / macos / windows
三个 `target_os`，**没有缺省分支**。在任何第四种目标上：

```text
error[E0308]: mismatched types
  --> crates/kaleido-adapter-codex/src/platform/mod.rs:20:52
     expected `Result<(), Error>`, found `()`
warning: unused variable: `command`
```

这**不影响**当前 macOS / Ubuntu CI（三个分支各自命中），但 **`target_os = "android"`
不等于 `"linux"`**，所以这段代码在 R3 的目标平台上根本不编译。

修法：给两个函数补一个**诚实的**缺省分支。`terminate_tree` 在不支持的平台上要返回明确的
`io::Error`，**不要**返回 `Ok(())` 假装杀干净了——那正是这个项目最不能接受的那类谎。
`configure` 可以是空实现，但要让参数名不触发 warning。

（放心：我已确认 `kaleido-core` 本身对 `aarch64-linux-android` 编译通过，
所以这条不影响你 T-102 的 UniFFI 结论。）

## 你已完成的部分我认可

callback interface、订阅 object、带 `CanonicalError` 的失败函数、返回 `CommandAck` 的
async 函数，两端都实际消费；Ubuntu Kotlin 与 macOS Swift 都是硬 CI 步骤无软化开关；
Windows 本地全绿。这些不用返工。

## 还差的三格，一格都不能省

1. macOS CI 上 Swift 编译成功的原始日志、`swiftc --version`、CI run URL、commit SHA；
2. **「故意改坏 Swift 导出 → CI 那一步真的变红」**的证据。没有这条，等于加了个永远不会
   报警的门禁——这一格我会重点看；
3. `docs/gates/T-102-evidence.md`，里面必须有一句明确结论：
   **「R3 的投影推送能不能走 UniFFI 回调？能／不能，理由是＿＿。」**

如果第 2 格因为 CI 往返成本太高不好做，可以在 macOS runner 上用同一条 `swiftc` 命令行
本地化验证并贴出来，但必须说明它与 CI 步骤是同一条命令。

继续吧。
