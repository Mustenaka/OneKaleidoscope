# 仓库收敛阻塞裁定 — 回给 Codex 的 prompt（原样粘贴）

> 2026-08-02。主管确认差异清单是自己写错的，已更正。
> 粘贴横线以内的全部内容。

---

## 裁定：5 个额外文档**全部保留**。清单是我写错的，不是你发现了异常。

你停下来上报是对的——边界写的是「若实际差异不是清单里的这些，先停下来」，
你照做了，而且备份、SHA-256 校验、`KEEP_COUNT=12` 都做完了才停，工作树零改动。
这正是要的行为。

**错在我。** 我做那次比对时，`T-103.md`、`T-103-codex-prompt.md`、`T-104.md`、
`T-104-codex-prompt.md`、`repo-convergence-prompt.md` 这 5 个文件**还不存在**——
我是在写完比对之后、同一轮里才创建它们的，却没有回头更新清单。

我已重新复核，结果与你的报告**逐项一致**：

```text
BRANCH_ONLY  = 1    .gitattributes
LOCAL_ONLY   = 13   （原 8 个 + 你列出的 5 个）
DIFFERENT    = 5    CLAUDE.md, docs/STATUS.md, docs/tasks/README.md,
                    docs/tasks/T-102.md, xtask/tests/deps.rs
SAME         = 479
```

## 更正后的清单

**按规则保留，不要按数字核对**——我还在继续写文档，任何写死的数字都会立刻过期。
（事实上写这份裁定本身又新增了 `docs/tasks/repo-convergence-reply.md`，
所以此刻是 **14 + 4 = 18** 个，而不是 17。这正是为什么下面那条规则比清单重要。）

复核时刻「只在主工作树」的 13 个（外加本文件）：

```text
docs/adr/0015-frozen-spike-tests-are-windows-only.md
docs/adr/0016-recorder-out-of-ci-and-single-backslash-roots.md
docs/adr/0017-line-ending-determinism.md
docs/gates/T-102-result.md
docs/tasks/T-102-unblock-reply.md
docs/tasks/T-102-unblock-reply-2.md
docs/tasks/T-102-unblock-reply-3.md
docs/tasks/T-102-unblock-reply-4.md
docs/tasks/T-103.md
docs/tasks/T-103-codex-prompt.md
docs/tasks/T-104.md
docs/tasks/T-104-codex-prompt.md
docs/tasks/repo-convergence-prompt.md
```

「主树更新」的 4 个不变：`CLAUDE.md`、`docs/STATUS.md`、`docs/tasks/README.md`、
`docs/tasks/T-102.md`。

`xtask/tests/deps.rs` 仍以**分支**为准（ADR-0017 的 fail-loud 修复）。
`.gitattributes` 随快进进来。

> 注意：`docs/tasks/repo-convergence-prompt.md` 本身也在保留清单里，
> 我刚刚更新了它（把 12 改成 17，并加了下面那条判定规则）。
> **请用工作树里的当前版本，不要用你第 1 步已经存到临时目录的那份旧副本。**

## 我给清单加了一条规则，免得这个时间差再发生

清单会随我继续写文档而变化，所以规则比清单权威：

> **`docs/**` 与根目录 `CLAUDE.md` 下只在主树存在或主树更新的文件，一律保留；
> 其余一切以分支为准。**

对应地，「什么时候该停下来」也改了：

- 差异属于「`docs/**` 或 `CLAUDE.md` 下多出的主管文档」→ **按规则直接保留并在交付里列出，不必停**；
- 差异出现在 `crates/`、`xtask/`、`schemas/`、`tests/fixtures/`、`spikes/` 等
  **任何非文档路径** → **必须停下来报告**。

## 继续执行

第 0 步的备份有效，不用重做。

**第 1 步请按规则重新枚举，而不是按我给的清单**：
把当前工作树里所有「只在主树存在」或「主树与分支内容不同」的文件，
凡是落在 `docs/**` 或根 `CLAUDE.md` 的，全部另存；
`xtask/tests/deps.rs` 不在其列（以分支为准）。
`repo-convergence-prompt.md` 要用工作树的**当前**版本，覆盖你第 1 步存的旧副本。

**第 4 步的校验改为规则式**（这是硬要求）：

- `git status --short` 的每一个条目都必须落在 `docs/**` 或根 `CLAUDE.md`；
- **出现任何非文档路径（`crates/`、`xtask/`、`schemas/`、`tests/fixtures/`、
  `spikes/`、`.gitattributes`、`Cargo.*`、`.github/`）→ 停下来报告**；
- 数量本身不再是判据，但请把实际数量和完整清单写进交付。

**第 5 步**：`git diff --stat 31c8a2d..HEAD` 必须只含 `docs/**` 与 `CLAUDE.md`，
不含任何非文档路径。

其余边界不变：`--ff-only` 拒绝就停；不许 `git clean -fdx`；
收敛后的 commit 必须自己拿到三平台 CI 全绿。

继续吧。
