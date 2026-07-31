# T-102 下发给 Codex 的 prompt（原样粘贴）

> **状态：queued。**
> 只有一个实现方时，**先做完 [T-100](T-100.md) 再下发本卡**；有第二个实现方时可并行。
> **硬约束：必须在 R3（Android 纵切）开工前完成。**

---

你是 OneKaleidoscope 项目的**实现方（Implementer）**。这张卡很小，但它决定两件事：
R1 能不能从「有条件通过」变成「通过」，以及 R3 的 Android 投影推送要不要换架构。

## 动手前必读

1. `AGENTS.md` —— 行为规范
2. `docs/tasks/T-102.md` —— **本次任务卡，DoD 是唯一验收标准**
3. `docs/adr/0013-platform-track-order.md` —— 为什么有这张卡
4. `docs/PROTOCOL.md` §2（R-P1）、§5.2、§6、§7、§8、§9
5. `crates/kaleido-core/src/lib.rs` 与两个 probe —— 现有探针长什么样
6. `docs/DEVELOPMENT.md` 的「R1 UniFFI binding probe」一节 —— 现有生成命令
7. `.github/workflows/ci.yml` —— 现有三平台矩阵

## 这张卡要解决的两个问题

**问题一：Swift 编译没有机器证据。**

R1 只做到「Swift 绑定生成成功」。生成 ≠ 编译。开发机是 Windows，本机和 WSL 都没有
Swift 工具链。但 `.github/workflows/ci.yml` 的矩阵**已经有 `macos-latest`**，
GitHub 的 macOS runner 自带 Swift。所以答案不是买 Mac，是在那个 job 上加一步。

**问题二：现有探针只验证了「值类型」，没验证「移动端真正的调用面」。**

现在的 `binding_probe` 是一个同步函数，进去 record 出来 record。但 R3 的 Android App
需要的是：

1. 投影被**推送**过来（`LiveActivityView` 的流式增量、`AttentionInboxView` 的新审批）；
2. 命令失败在 Kotlin/Swift 侧长什么样（抛异常？sealed result？）；
3. 一个**有状态的订阅句柄**，能订阅、能退订、进程被杀后能重建。

这三样 UniFFI 分别对应 callback interface、throwing function / async、object。
**一个都没验证过。** 如果其中哪个行不通，改的是架构，不是 UI。现在花半天知道，
比 R3 写到一半知道便宜得多。

## 具体做什么

### 1. 扩展 `crates/kaleido-core` 的探针

加四个导出，**全部不含业务逻辑**，只为让两端编译器检查形状：

- 一个 **callback interface**（外语言实现、Rust 调用），至少接收 `ProjectionEnvelope`
  和 `CanonicalError`；
- 一个 **object**（带构造和至少两个方法），一个方法接受上述 callback 完成"订阅"，
  另一个完成"退订"；
- 一个 **可失败函数**，错误载荷是 `CanonicalError`；
- 一个 **`async` 函数**，返回 `CommandAck`。

### 2. 两端消费探针都要真的调用

`bindings/kotlin-probe/src/main/kotlin/Probe.kt` 和 `bindings/swift-probe/Probe.swift`
各自：实现 callback、创建订阅对象、调用可失败函数并处理错误、await async 函数。
只 import 不调用不算。

### 3. CI 集成

在 `.github/workflows/ci.yml` 里加一步，只在 macOS 执行：构建 cdylib → 生成 Swift 绑定
→ `swiftc` 编译两份生成源和 `Probe.swift`。Kotlin 探针的编译也一并进 CI
（放哪个 os 你定，说明理由）。

**不得**加 `continue-on-error`、`|| true`、`if: false` 或任何软化开关。这一步必须是硬门禁。

### 4. 证明门禁真的有约束力

这是本卡最容易糊弄、也最关键的一条：临时给 `kaleido-core` 加一个 Swift 侧会编译失败的
导出，**证明 CI 那一步确实变红**，贴日志，然后移除。没有这条，等于加了一个永远不会
报警的门禁。

## 三条红线

**1. 不许改 `crates/kaleido-proto` 和 `docs/PROTOCOL.md`。**

如果 UniFFI 拒绝了某个 canonical 类型，或者 callback/object/async 跟现有类型合不来——
**停下来报告，不要改协议去迁就生成器**。交付时贴 `git diff --stat` 自证。

**2. 「UniFFI 做不到」是合格交付，不是失败。**

如果回调、object 或 async 在两端任意一端行不通，按 `AGENTS.md` §5 报告，给出：
被拒构造、生成器/编译器**原始错误**、最小复现、以及你认为可行的替代形状
（例如把推送降级为 `since_cursor` 轮询）。主管会据此决定改协议、改架构还是换方案。
**不要**自己发明一层影子 DTO 把问题盖过去。

**3. 不写任何产品逻辑。**

不建 Android/iOS 工程，不加 Gradle Android module，不建 Xcode project，
不在 `kaleido-core` 里写会话、投影计算或存储。
如果 [T-100](T-100.md) 正在并行进行，**不要碰它的四个 crate**。

## 交付要回答的那句话

`docs/gates/T-102-evidence.md` 里必须有一句明确结论：

> R3 的投影推送能不能走 UniFFI 回调？能／不能，理由是＿＿。

外加：Swift 编译成功的原始日志、runner 的 `swiftc --version`、CI run URL、commit SHA、
最终 workflow YAML，以及第 4 条的「改坏 → CI 变红」证据（单独成节）。

## 工程门禁

`cargo fmt --all -- --check`、`cargo xtask check-deps`、`cargo xtask lint-forbidden`、
`cargo clippy --all-targets -- -D warnings`、`cargo test --workspace`、`cargo xtask ci`
全绿。生成产物不入库。

现在开始。先看 `crates/kaleido-core/src/lib.rs` 和两个 probe，再看 UniFFI 0.32 关于
callback interface / object / async 的文档。
