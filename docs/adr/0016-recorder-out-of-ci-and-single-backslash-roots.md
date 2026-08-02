# ADR-0016: 冻结 recorder 完全退出 CI 测试门禁；泄漏扫描器识别单前导反斜杠根

- 状态：**已接受，2026-07-31**
- 决策人：项目主管
- 取代：[ADR-0015](0015-frozen-spike-tests-are-windows-only.md) **D-1**（D-2、D-3、D-4 继续有效）
- 触发：T-102 第三次阻塞

## 背景：ADR-0015 D-1 有一个我自己造成的漏洞

ADR-0015 D-1 规定「冻结 spike 的行为测试只在 Windows 上执行」。这条规则默认了
**Windows 会通过**。但实现方在上一轮阻塞报告里就已经写明：

```text
Windows GitHub runner:
outside_permission_target_is_rejected_with_redacted_structured_diagnostics ... FAILED
```

我把它登记成 D-B8 并标注「不阻塞」，理由是它不构成泄漏——**这个判断本身没错，但结论错了**：
它不是安全问题，却仍然是一条 CI 测试失败，因此三个平台全部红。ADR-0015 没有解决问题，
只是把阻塞从两个平台变成三个平台。这是我的疏漏，不是实现方的执行问题。

同一轮还暴露了第三处：`xtask/tests/fixtures.rs` 的
`sandbox_dot_segment_traversal_is_rejected_for_both_separators` 在 Unix 上失败。
这一处**性质完全不同**，见 D-2。

## 决策

### D-1 `kaleido-recorder` 完全退出 CI 测试门禁（取代 ADR-0015 D-1）

`cargo xtask ci` 与 `cargo xtask test` 在**所有平台**排除 `kaleido-recorder`：

```text
cargo test --workspace --exclude kaleido-recorder
```

排除仍然必须**显式打印**，不允许静默跳过。

**保留不变的**（三平台全部）：

- `cargo fmt --all -- --check`
- `cargo xtask check-deps`
- `cargo xtask lint-forbidden`
- **`cargo clippy --all-targets -- -D warnings`，仍然包含 `spikes/**`**
- **`cargo xtask fixtures verify`**
- `crates/**` 与 `xtask` 的**全部**测试

新增一条本地检查，写进 [DEVELOPMENT.md](../DEVELOPMENT.md)：

```text
cargo test -p kaleido-recorder      # Windows 本机，冻结 spike 的回归保护
```

### 为什么这次是「完全退出」而不是继续一处处让路

`kaleido-recorder` 已冻结，不会再演进。它留下的真正资产是**已提交的 fixture**，
而那些由 `cargo xtask fixtures verify` 在三个平台独立校验，与 recorder 能否跑测试无关。
`spikes/**` 仍在 clippy 覆盖内，所以它不会烂掉。

代价是：那 164 条行为测试从 CI 门禁降级为本地 Windows 检查。对一段**永不再改**的代码，
CI 回归保护的边际价值接近零；而它已经连续三轮阻塞产品主线，这正是 `CLAUDE.md` §4
「不得以修复 spike/recorder 为名推迟产品纵切」所指的情形。

我在上一轮已向负责人预告：若出现第三次阻塞，就重新评估是否把 recorder 整个移出，
而不是继续一处处让路。现在兑现。

### D-2 授权修复 `xtask` 泄漏扫描器：单前导反斜杠是根路径候选

这一处**不是** spike 问题，实现方的判断正确，予以批准。

`xtask/src/fixtures.rs` 的 `absolute_path_candidates` 目前只识别三种绝对路径：

| 形态 | 识别函数 |
|---|---|
| `C:\...` / `C:/...` | `is_windows_drive_absolute` |
| `\\server\share` | `is_unc_absolute`（要求**两个**反斜杠） |
| `/unix/path` | `is_unix_absolute_with_segments` |

**单个**前导反斜杠的 `\tmp\foo\secret.txt` 不被识别。这在 Windows 上是合法的
「当前驱动器根」绝对路径形态，因此一份含有该形态路径的 fixture 会**整个绕过泄漏扫描**。

`cargo xtask fixtures verify` 是三平台常驻的、对已提交证据生效的泄漏门禁，
不是冻结 spike。泄漏扫描器的正确偏置是 fail-closed：**多识别一个候选的代价是一次误报，
少识别一个的代价是提交的证据里漏出真实路径。** 因此这是真实缺口，值得修。

授权范围严格限定：

**允许**：在 `absolute_path_candidates` / `is_unc_absolute` 一线，**增加**对单前导反斜杠
根形态的识别。

**禁止**：

- 改动 sandbox 内外比较逻辑（`path_is_inside_sandbox`）与占位符处理
  （`path_start_boundary` 的 `<SANDBOX>` / `<HOME>` / `<OUTSIDE_PATH>` 抑制）；
- 改动任何断言；
- 让 UNC（`\\`）的既有行为发生变化。

**必须证明**：

1. 新识别**真的会咬**：加一条测试，让一个 sandbox 之外的 `\foo\secret.txt` 被标记为
   `leak: absolute path outside fixture sandbox`；改坏识别后该测试变红；
2. **没有引入误报**：`cargo xtask fixtures verify` 在三个平台上对真实的
   5 文件 / 220 条记录仍然通过；
3. sandbox **之内**的单反斜杠路径不被误报。

### D-3 D-B6 / D-B7 / D-B8 仍然未修复

本 ADR 不修复它们中的任何一条：

- **D-B6 / D-B7**（Unix 上 `\` 的语义、macOS `/var` 符号链接别名）：仍是 **R9 前置**。
  D-2 只让泄漏扫描器多识别一种**候选形态**，没有回答「`\` 在 Unix 上算不算分隔符」
  这个语义问题——那个决定留给 R9 真正写路径校验时。
- **D-B8**（`<HOME>` 先于 `<SANDBOX>` 命中）：随 recorder 一起退出 CI，
  **仍然未修复**，R4 推送/relay 脱敏定稿时复查。

任何文档、任何交付说明都不得把这三条写成已解决。

## 后果

- T-102 终于可以跑到 Swift 与 Kotlin 编译门禁；
- `xtask` 需要两处改动：排除范围（D-1）与泄漏扫描器识别（D-2），**分开列出**；
- [DEVELOPMENT.md](../DEVELOPMENT.md) 增加 recorder 的本地 Windows 检查说明。

## 教训

ADR-0015 D-1 是在「本机 Windows 全绿」的基础上写的，而实现方在同一份报告里已经给出了
Windows runner 失败的原始输出。**裁定一个门禁的范围时，必须把已知的失败清单逐条对照过，
而不是只对照自己关注的那两个平台。**
