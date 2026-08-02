# ADR-0015: 冻结 spike 的行为测试只在 Windows 上执行；编译与 lint 仍然全平台

- 状态：**D-1 已被取代（2026-07-31 同日）；D-2 / D-3 / D-4 仍然有效**
- 取代者：[ADR-0016](0016-recorder-out-of-ci-and-single-backslash-roots.md) D-1
- 取代原因：D-1 默认「Windows 会通过」，但 Windows GitHub runner 因 D-B8 同样失败，
  于是三个平台全红。这是主管的疏漏，详见 ADR-0016 背景。
- 决策人：项目主管
- 触发：T-102 第二次阻塞——三平台 CI 首次真跑，冻结的 `kaleido-recorder` 出现三处
  平台相关的测试失败，挡住了 macOS 上的 Swift 编译门禁
- 影响：`cargo xtask ci` / `cargo xtask test` 的执行范围
- 不影响：`crates/**` 的任何测试、`xtask` 的任何测试、`cargo xtask fixtures verify`

## 背景

本仓库此前从未推送，2026-07-31 是三平台 CI 第一次真正运行。修完已授权的条件编译问题后，
`spikes/kaleido-recorder` 暴露出三处**运行时语义**失败：

| 平台 | 失败 | 根因 |
|---|---|---|
| Ubuntu / macOS | `permission_scope_rejects_traversal_absolute_outside_and_placeholders` | Unix 把 `..\outside.txt` 里的反斜杠当普通文件名字符，因此它**确实**没有逃逸出 sandbox。断言编码的是 Windows 的路径语义 |
| macOS | `exact_permission_path_accepts_safe_dotdot_normalization` | macOS 临时目录 `/var/...` 是系统级符号链接别名，校验器先把 `/var` 祖先判为不安全链接 |
| Windows（GitHub runner） | `outside_permission_target_is_rejected_with_redacted_structured_diagnostics` | runner 的临时目录落在 `<HOME>` 之下，脱敏时 `<HOME>` 先于 `<SANDBOX>` 命中。**仍然完成了脱敏，没有泄漏**，只是标签不够具体 |

第三条与 R1 记录的 D-R1-2 是同一类问题——那次已经做过一次授权的窄范围修正。同一个测试
因为「临时目录落在哪」再次失败，说明它对执行环境敏感，而不是产品逻辑有缺陷。

主管核实的两个关键事实：

1. **产品代码没有同类逻辑。** `crates/**` 中不存在权限路径穿越校验；
   `kaleido-hostd` 只对 `project_root` 做一次 `canonicalize` 供自己使用。
2. **产品代码不依赖 recorder。** 依赖规则本就禁止，实际代码中也不存在引用。

## 决策

### D-1 冻结 spike 的行为测试只在 Windows 上执行

`cargo xtask ci` 与 `cargo xtask test` 在非 Windows 平台上排除 `kaleido-recorder`：

```text
cargo test --workspace --exclude kaleido-recorder     # 非 Windows
cargo test --workspace                                # Windows
```

排除必须**在日志里显式打印**，例如
`test: kaleido-recorder excluded on this platform (ADR-0015)`，
不允许静默跳过。Windows 仍然全量执行——若它在 Windows 上坏了，我们照样会知道。

### D-2 编译与 lint 仍然全平台覆盖，一格不减

以下在三个平台上都不变：

- `cargo fmt --all -- --check`
- `cargo xtask check-deps`
- `cargo xtask lint-forbidden`
- **`cargo clippy --all-targets -- -D warnings`（包含 `spikes/**`）**
- **`cargo xtask fixtures verify`**
- `crates/**` 与 `xtask` 的**全部**测试

界线是：**冻结的 spike 必须在所有平台上编译干净；它的行为测试只需要在它真正会运行的
平台上通过。** clippy 覆盖不能减——正是它抓到了上一轮的 cfg 缺陷，那些值得修且很便宜。

`fixtures verify` 尤其不能减：committed fixture 才是 recorder 留下的真正资产，
它的校验与 recorder 能否在 macOS 上跑测试完全无关。

### D-3 这不是降低需求，理由必须写清楚

`kaleido-recorder` 是**已冻结的研究 spike**（[STATUS](../STATUS.md) §4：
「保留为一手证据和研究资产，不再作为开始产品代码的前置门禁」）。它在 Windows 上开发，
只在 Windows 上运行过一次以录制 Codex 报文，此后不再演进，也**没有任何计划**让它在
macOS 或 Linux 上运行。

「冻结的 Windows 专用研究工具的测试在 macOS 上通过」从来不是本项目的需求，它是三平台
CI 矩阵带来的**意外门禁**。取消一个意外门禁不是降低需求。

反过来，`CLAUDE.md` §4 明确禁止「以修复 spike/recorder 为名长期推迟产品纵切」——
为了让一个冻结工具在两个它永远不会运行的平台上通过测试，而卡住 R8 的 Swift 证据，
正是那条禁令针对的情形。

### D-4 三处失败作为观察记录保留，不得当作已修复

| ID | 观察 | 处置 |
|---|---|---|
| **D-B6** | 路径校验若要跨平台，必须显式决定 `\` 在 Unix 上算普通字符还是分隔符。安全校验器倾向 fail-closed（两者都当分隔符），代价是拒绝合法的 Unix 文件名 | **产品相关**。`crates/**` 目前没有路径校验；等 R9（文件树 / 只读预览 / Git）真正需要时，必须先决定这条规则再写代码，不得照抄 recorder |
| D-B7 | macOS 的 `/var` 是系统符号链接别名，任何「祖先链接即不安全」的校验都会在 macOS 临时目录上误判 | 同上，R9 的路径校验必须处理 |
| D-B8 | 脱敏占位符存在优先级问题：`<HOME>` 会先于 `<SANDBOX>` 命中。**不是泄漏**，是标签精度 | 产品脱敏在 `crates/kaleido-state` 中是另一套实现，已有真实日志扫描测试。R4 推送/relay 脱敏定稿时复查一次 |

这三条都**没有被修复**，只是不再阻塞。任何文档都不得把它们写成已解决。

## 后果

- T-102 得以继续，Swift 与 Kotlin 编译门禁能真正跑到；
- `xtask` 的 `ci` / `test` 需要按 D-1 修改，并补一条测试证明排除逻辑本身正确；
- D-B6 / D-B7 进入 R9 前置清单，D-B8 进入 R4 复查清单。

## 被否决的方案

| 方案 | 否决理由 |
|---|---|
| 授权修改冻结 recorder 的生产权限校验，让 `\` 在 Unix 上也算分隔符 | 需要对一个**冻结的安全校验器**做三处独立语义判断，产品代码却完全不用它。投入产出严重不成比例，且改动的是安全敏感代码 |
| 逐个把三个失败测试标成 `#[cfg(windows)]` | 更外科手术，但对失败 1 是**错的**——那条断言表达的是平台无关的安全意图，标成 Windows-only 等于把设计意图改写成平台事实。而且仍要动三处冻结代码 |
| 放宽这三个断言 | `AGENTS.md` §2.3 红线，且实现方已经正确拒绝了这条路 |
| 把 recorder 移出 workspace | 会同时丢掉 clippy 与编译覆盖，比本决策更激进，收益并不更多 |
| 让 CI 只跑 Windows | 直接放弃 macOS Swift 与 Ubuntu Kotlin 门禁，正是 T-102 要建立的东西 |
