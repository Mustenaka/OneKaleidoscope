# T-102 第二次阻塞裁定 — 回给 Codex 的 prompt（原样粘贴）

> 2026-07-31，主管对三处冻结 recorder 运行时失败的裁定。
> 粘贴横线以内的全部内容。

---

## 裁定：三条建议全部**不批准**。不是因为你判断错了，是因为问题问错了层。

先说你做对的：三处失败你都定位准确、根因分析正确，而且**没有**改断言、没有 `#[ignore]`、
没有调 CI 顺序绕过。上报是对的。已授权的 cfg 修复也一次做完了，我核实过范围。

但你的三条建议都是「怎么修 recorder」。真正该问的是：**recorder 为什么在挡 Swift 门禁？**

## 我核实的两个事实

1. **产品代码没有同类逻辑。** `crates/**` 里不存在权限路径穿越校验；
   `kaleido-hostd` 只对 `project_root` 做一次 `canonicalize` 供自己使用。
2. **产品代码不依赖 recorder。** 依赖规则本就禁止，实际代码里也没有引用。

`spikes/kaleido-recorder` 是**已冻结的研究 spike**，在 Windows 上开发，只在 Windows 上跑过
一次以录制 Codex 报文，此后不再演进，也没有任何计划让它在 macOS 或 Linux 上运行。

「冻结的 Windows 专用研究工具的测试在 macOS 上通过」从来不是本项目的需求——它是三平台
CI 矩阵带来的**意外门禁**。为了让它在两个永远不会运行的平台上变绿，而卡住 R8 的 Swift
证据，正是 `CLAUDE.md` §4「不得以修复 spike/recorder 为名推迟产品纵切」针对的情形。

## 决定：见 [ADR-0015](../adr/0015-frozen-spike-tests-are-windows-only.md)

### 你要改的（`xtask`，本卡授权）

`cargo xtask ci` 与 `cargo xtask test` 在非 Windows 平台排除 `kaleido-recorder`：

```text
cargo test --workspace --exclude kaleido-recorder     # 非 Windows
cargo test --workspace                                # Windows
```

两条硬要求：

1. **排除必须在日志里显式打印**，例如
   `test: kaleido-recorder excluded on this platform (ADR-0015)`。
   静默跳过等于把门禁藏起来，那才是我要打回的东西。
2. **补一条测试证明排除逻辑本身正确**——即它在 Windows 上确实不排除。

### 一格都不许减的（三平台全部保留）

- `cargo fmt --all -- --check`
- `cargo xtask check-deps`
- `cargo xtask lint-forbidden`
- **`cargo clippy --all-targets -- -D warnings`，包含 `spikes/**`**
- **`cargo xtask fixtures verify`**
- `crates/**` 与 `xtask` 的**全部**测试

界线是：**冻结的 spike 必须在所有平台上编译干净；它的行为测试只需要在它真正会运行的
平台上通过。** clippy 覆盖绝不能减——正是它抓到了上一轮那些 cfg 缺陷。
`fixtures verify` 更不能减：committed fixture 才是 recorder 留下的真正资产，
它的校验跟 recorder 能不能在 macOS 上跑测试毫无关系。

## 三处失败**没有被修复**，只是不再阻塞

我把它们登记成了观察项。任何文档、任何交付说明都不得把它们写成已解决：

| ID | 观察 | 归属 |
|---|---|---|
| **D-B6** | 跨平台路径校验必须显式决定 `\` 在 Unix 上算普通字符还是分隔符。安全校验器倾向 fail-closed，代价是拒绝合法的 Unix 文件名 | **R9 前置**。等文件树 / 只读预览 / Git 真正需要路径校验时，先定规则再写代码，**不得照抄 recorder** |
| D-B7 | macOS 的 `/var` 是系统符号链接别名，「祖先是链接即不安全」的校验在 macOS 临时目录上必然误判 | R9 前置 |
| D-B8 | 脱敏占位符有优先级问题：`<HOME>` 先于 `<SANDBOX>` 命中。**这不是泄漏**，是标签精度 | R4 推送/relay 脱敏定稿时复查 |

顺带说一句：你对失败 1 的分析我同意一半。Unix 上 `..\outside.txt` 确实是**一个**文件名，
所以现有校验接受它在 Unix 语义下是**正确的**——是那条断言编码了 Windows 的路径语义。
你提的 fail-closed 改法在安全校验器里是站得住的选择，但那是 D-B6 要在 R9 做的决定，
不该在一个冻结 spike 上先斩后奏。

## 回到 T-102 本身

改完 xtask 后，你还差的就是原来那三格，一格都不能省：

1. macOS CI 上 Swift 编译成功的原始日志、`swiftc --version`、CI run URL、commit SHA；
2. **「故意改坏 Swift 导出 → CI 那一步真的变红」**的证据。没有这条，等于加了个永远不会
   报警的门禁——我会重点看这一格；
3. `docs/gates/T-102-evidence.md`，里面必须有一句明确结论：
   **「R3 的投影推送能不能走 UniFFI 回调？能／不能，理由是＿＿。」**

交付时照旧：逐条列出 xtask 的改动、重建冻结区哈希基线、
证明 Windows 上 `cargo xtask ci` 前后都 exit 0 且测试计数不变。

继续吧。这次应该能跑到 Swift 了。
