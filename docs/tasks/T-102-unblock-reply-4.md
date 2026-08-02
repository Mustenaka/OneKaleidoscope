# T-102 第四次阻塞裁定 — 回给 Codex 的 prompt（原样粘贴）

> 2026-07-31，主管裁定。批准，但采用你**不推荐**的那个方案，理由在下面。
> 粘贴横线以内的全部内容。

---

## 先说：核心证据你已经拿到了

macOS CI 上 Swift 6.3.2 编译成功、Ubuntu Kotlin 成功、四类调用面两端实际消费并编译，
以及变异 run 30614223755 的 `invalid redeclaration of 'probeProtocolVersion()'` ——
**「改坏 Swift 导出 → CI 那一步真的变红」这一格拿到了**，这是我最看重的一格。
UB-R1-S 与 G-R1-1 的实质工作已经完成。剩下的是收尾。

## 裁定：批准修复，但用 `.gitattributes`，不用测试内规范化

你的诊断正确。我核实了 `xtask/src/deps.rs` 用 `source.lines()`（Rust 的 `lines()` 会剥掉
尾部 `\r`）并对每段 `.trim()`，所以**生产依赖检查器确实是 CRLF 安全的**，问题只在测试。

但你把 `.gitattributes` 判为「影响范围更大，不推荐」——范围确实更大，而那正是需要的。
你没有看到下面这一层：

`xtask/src/schema.rs:874` 的 `git_blob_digest` 对 `schemas/**` 已提交快照计算
**原始工作树字节**的 git blob SHA-1：

```rust
let bytes = fs::read(source.join(file))?;
let actual = git_blob_digest(&bytes);
```

git 自己的 blob 哈希是对**规范化后（LF）**的内容算的。在 `core.autocrlf=true` 且仓库没有
`.gitattributes` 时，全新 Windows clone 的工作树是 CRLF，于是
`git_blob_digest(工作树字节) ≠ git 存储的 blob 哈希` ——
**`cargo xtask schema diff` 会在每一次全新 Windows clone 上报假漂移。**

这直接削弱 ADR-0012 D-2 的漂移守卫，也就是整个「钉定路径解码、不生成上游类型」策略的
机器保障。它今天没炸只是因为 `schema diff` 不在 `cargo xtask ci` 里，
但 T-100 §6.1 要求真实验收前先跑它。

**本仓库的核心资产是逐字节精确的已提交快照与 fixture。这种仓库不能让行尾翻译听天由命。**

### 你要做的（[ADR-0017](../adr/0017-line-ending-determinism.md)）

**D-1：新增 `.gitattributes`**

```gitattributes
* text=auto eol=lf

# 逐字节证据：永不做行尾翻译
schemas/**          -text
tests/fixtures/**   -text
```

`schemas/**` 与 `tests/fixtures/**` 用 `-text` 而非继承 `eol=lf`，因为对一手证据最强的
保证不是「翻译成 LF」，而是「任何情况下都不翻译」。

首次 checkout 可能出现一次性行尾规范化改动，属预期。但要**明确确认**：
`schemas/**` 与 `tests/fixtures/**` 的**内容字节未变**（`git diff --stat` 对这两个
目录为空），否则停下来报告——那意味着它们此前就已经被翻译过，是另一个问题。

**D-2：测试 setup 必须 fail-loud**

只把 CRLF 规范化成 LF 不够。一个 `.replace()` 静默未命中，会让测试**以错误的理由**通过或
失败——这和 `DEVELOPMENT.md` 里脱敏扫描必须先断言「捕获非空」是同一条原则。

所以修复必须包含**断言替换确实发生了**（替换前后内容不同，或目标子串确实存在）。
规范化 CRLF 可以一并做，**但载荷是那条断言**：setup 没生效时要立刻以清晰信息失败，
而不是带着未生效的模拟规则继续跑。

不许改该测试的业务断言，不许改依赖检查器语义。

**D-3：已登记 D-B10** —— 下次真正跑 `cargo xtask schema diff` 时，
必须复核它在干净 Windows clone 上不再假报。

## 另一件必须处理的事：证据文件的位置和分支状态

`T-102-evidence.md` 现在只存在于
`target/t102-swift-mutation/docs/gates/T-102-evidence.md` —— 那是 gitignored 的临时
worktree。主工作树的 `docs/gates/` 里**没有**它。

同时主工作树目前的状态是：`main` 停在 `ae9da23`（R1 之前），**37 个未提交路径**，
里面是 R1、R2（T-100 的四个 crate）和 T-102 期间新增的全部文档；
而 `origin/codex/t-102-uniffi-probe` 上是你提交的快照 `6de6eb0`。

**两个来源都不完整**：你的分支没有主管在 T-102 期间写的
ADR-0015 / ADR-0016 / ADR-0017、T-102 §5.4～§5.6 的授权正文、以及 D-B6～D-B10 的登记。

所以交付时请：

1. 把 `docs/gates/T-102-evidence.md` 落到**主工作树的** `docs/gates/` 下，不要留在
   `target/` 里；
2. 明确说明你的分支与主工作树之间的差异，**不要**自行合并或覆盖主工作树的文档——
   由负责人决定怎么收敛。

## 收尾还差的

1. Windows 干净 checkout 上 `cargo xtask ci` 全绿，含 `fixtures verify` 的
   `5 files / 220 records`；
2. 三平台同一 commit 全绿的 CI run URL 与 SHA；
3. `docs/gates/T-102-evidence.md` 落到主工作树，里面必须有那句明确结论：
   **「R3 的投影推送能不能走 UniFFI 回调？能／不能，理由是＿＿。」**
   ——你已经让四类调用面在两端编译并被消费，那么这句话就该是一个有依据的「能」，
   把依据写具体（哪个 UniFFI 特性、两端各自的形态、有没有限制）。

D-B6 / D-B7 / D-B8 继续保持未修复，不许写成已解决。

## 最后

四轮阻塞你都定位准确、没有一次用 `allow` / `#[ignore]` / 调顺序绕过。
这四轮暴露的是「仓库从未推送过」积累的债——本地工作树的偶然状态（LF、临时目录位置、
已装工具链）被当成了代码的真实前提。这不是你的执行问题。
