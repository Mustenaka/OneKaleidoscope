# ADR-0017: 用 `.gitattributes` 固定行尾；测试的 setup 必须 fail-loud

- 状态：**已接受，2026-07-31**
- 决策人：项目主管
- 触发：T-102 第四次阻塞（Windows 干净 checkout 上 `xtask/tests/deps.rs` 失败）
- 影响：新增 `.gitattributes`；`xtask/tests/deps.rs` 的 setup 断言
- 不影响：`crates/kaleido-proto`、`PROTOCOL.md`、committed fixture 与 schema 的**内容**

## 背景

实现方在 `core.autocrlf=true` 的全新 Windows worktree 上复现：
`xtask/tests/deps.rs:294` 对 `include_str!("../../docs/dependency-rules.toml")` 做含 `\n`
的精确 `.replace()`，而干净 checkout 得到的是 CRLF，替换**静默未命中**，
于是测试的模拟规则没生效，断言在下游失败。

主管核实了两件事：

1. **生产依赖检查器是 CRLF 安全的**：`xtask/src/deps.rs` 用 `source.lines()`
   （Rust 的 `lines()` 会剥掉尾部 `\r`）并对每个片段 `.trim()`。所以这确实只是测试缺陷。
2. **但根因不止于此，而且更严重**——见下。

## 实现方没有看到的那一层

`xtask/src/schema.rs:874` 的 `git_blob_digest` 对 `schemas/**` 的已提交快照计算
**原始工作树字节**的 git blob SHA-1：

```rust
let bytes = fs::read(source.join(file))?;
let actual = git_blob_digest(&bytes);
```

git 自己的 blob 哈希是对**规范化后（LF）**的内容算的。在 `core.autocrlf=true` 且仓库
**没有 `.gitattributes`** 的情况下，全新 Windows clone 的工作树是 CRLF，于是
`git_blob_digest(工作树字节) ≠ git 存储的 blob 哈希` ——
**`cargo xtask schema diff` 会在每一次全新 Windows clone 上报告假漂移。**

这直接削弱 [ADR-0012](0012-provider-decode-strategy.md) D-2 的漂移守卫，而那是整个
「钉定路径解码、不生成上游类型」策略的机器保障。它今天没有炸，只是因为
`schema diff` 不在 `cargo xtask ci` 的步骤里；但 [T-100](../tasks/T-100.md) §6.1
明确要求真实验收前先跑它。

本仓库的核心资产是**逐字节精确的已提交快照与 fixture**。这种仓库不能让行尾翻译听天由命。

## 决策

### D-1 新增 `.gitattributes`，工作树行尾在所有平台上确定为 LF

```gitattributes
* text=auto eol=lf

# 逐字节证据：永不做行尾翻译
schemas/**          -text
tests/fixtures/**   -text
```

`schemas/**` 与 `tests/fixtures/**` 用 `-text` 而不是继承 `eol=lf`，是因为它们是
**一手证据**：对它们最强的保证不是「翻译成 LF」，而是「任何情况下都不翻译」。

实现方判断 `.gitattributes`「影响范围更大，不推荐」——范围确实更大，但那正是需要的。
它是唯一能同时解决测试脆弱、schema 假漂移，并阻止这一类问题复发的改动。

### D-2 测试的 setup 必须 fail-loud，不许静默无效

只把 CRLF 规范化成 LF 是不够的。一个 `.replace()` 静默未命中，会让测试**以错误的理由**
通过或失败——这与 [DEVELOPMENT.md](../DEVELOPMENT.md) 里脱敏扫描必须先断言「捕获非空」
是同一条原则。

因此 `xtask/tests/deps.rs` 的修复必须包含：**断言替换确实发生了**
（例如替换前后内容不同，或目标子串确实存在）。规范化 CRLF 可以一并做，
但**载荷是那条断言**：setup 没生效时，测试要立刻以清晰信息失败，而不是带着未生效的
模拟规则继续跑下去。

不许改动该测试的业务断言，也不许改依赖检查器语义。

### D-3 登记 D-B10

| ID | 内容 | 处置 |
|---|---|---|
| **D-B10** | `cargo xtask schema diff` 对工作树字节算 git blob 哈希，在无 `.gitattributes` 的 Windows clone 上会报假漂移 | 由本 ADR D-1 消除。**下次真正跑 `schema diff` 时必须复核**它在干净 Windows clone 上不再假报 |

## 后果

- 新增 `.gitattributes` 后首次 checkout 可能出现一次性的行尾规范化改动，属预期；
- `xtask/tests/deps.rs` 增加 setup 断言；
- ADR-0012 D-2 的漂移守卫在 Windows 上恢复可信。

## 教训

这是 T-102 的第四次阻塞，四次全部来自「仓库从未推送过」——本地工作树的偶然状态
（LF、临时目录位置、已装工具链）被当成了代码的真实前提。
**一手证据型仓库必须尽早推送并让 CI 真跑**，越晚越贵。
