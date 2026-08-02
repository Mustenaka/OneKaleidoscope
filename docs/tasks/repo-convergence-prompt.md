# 仓库收敛 — 下发给 Codex 的 prompt（原样粘贴）

> 2026-08-02。这不是产品任务卡，是一次性的仓库状态收敛。
> 主管已把差异摸清楚：分支是 `main` 的直系后代，可以**快进**，没有合并冲突。
> 粘贴横线以内的全部内容。

---

你是 OneKaleidoscope 项目的**实现方（Implementer）**。这次不写产品代码，
做一次仓库状态收敛。仓库在 `D:\Work\Code\Cross\OneKaleidoscope`，Windows。

## 现状：两个都不完整的来源

| 来源 | 状态 |
|---|---|
| 本地 `main` 工作树 | HEAD 停在 `ae9da23`，工作树里有 R1/R2/T-100/T-102 的全部内容（大部分**未跟踪**） |
| `origin/codex/t-102-uniffi-probe` | tip `31c8a2d`，13 个提交，**已由三平台 CI 验证全绿** |

主管已经逐文件比对过，差异**很小且已完全确定**：

**分支是 `ae9da23` 的直系后代** —— `git merge-base --is-ancestor ae9da23 <branch>` 为真，
所以可以 `--ff-only` 快进，**不存在合并冲突**。

**判定规则（比清单更权威——清单会随主管继续写文档而变化）：**

> **`docs/**` 与根目录 `CLAUDE.md` 下只在主树存在或主树更新的文件，一律保留；
> 其余一切以分支为准。**

按此规则，2026-08-02 复核的实际清单：

| 类别 | 数量 | 清单 |
|---|---|---|
| 内容完全相同 | 479 | 无需处理 |
| **只在分支上** | 1 | `.gitattributes` |
| **只在主工作树** | 13 | `docs/adr/0015-*.md`、`0016-*.md`、`0017-*.md`；`docs/gates/T-102-result.md`；`docs/tasks/T-102-unblock-reply.md`、`-2`、`-3`、`-4`；`docs/tasks/T-103.md`、`T-103-codex-prompt.md`、`T-104.md`、`T-104-codex-prompt.md`、`repo-convergence-prompt.md` |
| **内容不同：主树更新** | 4 | `CLAUDE.md`、`docs/STATUS.md`、`docs/tasks/README.md`、`docs/tasks/T-102.md` |
| **内容不同：分支更新** | 1 | `xtask/tests/deps.rs`（ADR-0017 的 fail-loud 修复） |

合计要保留的主树文档 = **13 + 4 = 17 个文件**。其余一切以分支为准。

> **2026-08-02 更正**：本清单初版写的是「8 + 4 = 12」。那是主管在创建
> `T-103`/`T-104` 两张卡及其 prompt、以及本文件**之前**做的比对，随后又新增了 5 个文档
> 却没有更新清单。实现方按边界停下来上报，是正确的。上面的判定规则就是为了让这类
> 时间差能自解决。

（`docs/草稿/idea` 两边都有，之前的比对脚本因路径编码把它同时报进了两个「只在」清单，
是误报，不用处理。）

## 目标形态

```
main  ──快进──►  31c8a2d（分支 tip，CI 已验证）
                     └── 新增一个提交：主管在 T-102 期间写的 12 个文档
```

## 执行步骤

### 第 0 步：备份（不许跳过）

把整个工作树复制一份到仓库**外面**（排除 `target/`）。这是唯一一份包含主管文档的副本，
在它安全落地之前不要执行任何 `checkout` / `reset` / `clean`。

复制完成后确认副本里有那 8 个 ADR/gate/reply 文件，再往下走。

### 第 1 步：把 17 个要保留的文件另存

把上表里「只在主工作树」的 13 个 + 「主树更新」的 4 个，共 17 个文件，
复制到一个临时目录（例如 `%TEMP%\kaleido-docs-keep\`，保持相对路径）。

### 第 2 步：清理工作树，让快进能进行

1. 丢弃所有**已跟踪**文件的本地修改：`git checkout -- .`
   （其中 4 个文档你已在第 1 步存好；`xtask/tests/deps.rs` 本来就该以分支为准）
2. 删除那些**未跟踪但分支上也有**的文件，否则 git 会报
   "untracked working tree files would be overwritten by merge"。
   逐个按分支文件清单删，**不要**用 `git clean -fdx` ——
   它会连 `.claude/settings.local.json` 这类被忽略的本地配置一起删掉。

### 第 3 步：快进

```bash
git fetch origin
git merge --ff-only origin/codex/t-102-uniffi-probe
```

必须是 `--ff-only`。如果它拒绝，**停下来报告**，不要改成普通 merge 或 rebase。

快进后确认：`git status` 干净（除了被忽略的本地配置），`git log --oneline -1` 是 `31c8a2d`。

### 第 4 步：把 17 个文档放回并提交

1. 从临时目录复制回原路径；
2. **规则式校验**（数量本身不是判据——主管还在继续写文档）：
   `git status --short` 的每一个条目都必须落在 `docs/**` 或根 `CLAUDE.md`。
   **出现任何非文档路径（`crates/`、`xtask/`、`schemas/`、`tests/fixtures/`、
   `spikes/`、`.gitattributes`、`Cargo.*`、`.github/`）→ 停下来报告。**
   把实际数量与完整清单写进交付；
3. `git add` 这些文档并提交。提交信息写清楚这是主管在 T-102 期间产出的文档，
   并列出 ADR-0015/0016/0017、T-102 门禁结果、以及 T-103/T-104 两张排队卡。

### 第 5 步：验证

- [ ] `cargo xtask ci` exit 0，并贴出六道门禁的输出；
- [ ] `docs/gates/T-102-evidence.md` 现在是**已跟踪**文件（此前是 untracked）；
- [ ] `git diff --stat 31c8a2d..HEAD` 只含 `docs/**` 与根 `CLAUDE.md`，**不含**
      `crates/`、`xtask/`、`schemas/`、`tests/fixtures/`、`.gitattributes` 的任何改动；
- [ ] `schemas/**` 与 `tests/fixtures/**` 相对 `31c8a2d` **字节零改动**
      （`.gitattributes` 已经把它们标成 `-text`，所以放回文档不应该触发任何重规范化。
      若出现改动，**停下来报告**）。

### 第 6 步：推送并等 CI

推 `main`，然后等三平台 CI 在**这个新提交**上跑完。

只有 CI 在收敛后的 commit 上全绿，收敛才算完成——不能引用分支上那次绿来代替。
若变红，如实报告，不要回滚掩盖。

## 边界

- **不许改写分支历史**，不许 force-push；
- **不许**修改 `crates/**`、`xtask/**`、`schemas/**`、`tests/fixtures/**`、
  `docs/PROTOCOL.md`、`docs/adr/0012~0017` 的任何内容——本次只搬运文档；
- 不许「顺手」修 D-B6 / D-B7 / D-B8 / D-B11，它们仍是已登记的未修复项；
- 不许删除 `docs/tasks/T-102-unblock-reply*.md`，那是四次裁定的记录。

## 交付

1. 每一步的真实输出（尤其第 4 步的 `git status --short` 和第 5 步的四项验证）；
2. 收敛后 commit 的 SHA 与三平台 CI run URL；
3. 任何与上表不符的发现。**若差异属于「`docs/**` 或 `CLAUDE.md` 下多出的主管文档」，
   按上面的判定规则直接保留并在交付里列出即可，不必停下来**；
   若差异出现在 `crates/`、`xtask/`、`schemas/`、`tests/fixtures/`、`spikes/` 等任何
   非文档路径，**必须停下来报告**，不要自行判断该保留哪边。

现在开始。第 0 步的备份不许跳过。
