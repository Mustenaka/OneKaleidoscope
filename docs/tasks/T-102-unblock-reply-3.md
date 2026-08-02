# T-102 第三次阻塞裁定 — 回给 Codex 的 prompt（原样粘贴）

> 2026-07-31，主管裁定。含一处主管自己的错误更正。
> 粘贴横线以内的全部内容。

---

## 两件事：一件你是对的，我批准；一件是我上一轮判错了，我改。

### 1. 你的建议批准。这次你判断正确。

我核对了 `xtask/src/fixtures.rs`：`absolute_path_candidates` 只识别 `C:\...`、
`\\server\share`（`is_unc_absolute` 要求**两个**反斜杠）、`/unix/path` 三种形态。
**单个**前导反斜杠的 `\tmp\foo\secret.txt` 确实不被识别。

关键在于这不是 spike 问题：`cargo xtask fixtures verify` 是三平台常驻的、对已提交证据
生效的泄漏门禁。而 `\foo\bar` 在 Windows 上是合法的「当前驱动器根」绝对路径形态——
一份含该形态路径的 fixture 会**整个绕过泄漏扫描**。

泄漏扫描器的正确偏置是 fail-closed：**多识别一个候选的代价是一次误报，少识别一个的代价
是提交的证据里漏出真实路径。** 这是真实缺口，改。

授权范围严格限定（[ADR-0016](../adr/0016-recorder-out-of-ci-and-single-backslash-roots.md) D-2）：

**允许**：在 `absolute_path_candidates` / `is_unc_absolute` 一线**增加**对单前导反斜杠根的识别。

**禁止**：动 `path_is_inside_sandbox`；动 `path_start_boundary` 里
`<SANDBOX>` / `<HOME>` / `<OUTSIDE_PATH>` 的抑制逻辑；动任何断言；让 UNC（`\\`）行为改变。

**必须证明三件事**：

1. 新识别**真的会咬** —— 加一条测试，让 sandbox 之外的 `\foo\secret.txt` 被标记为
   `leak: absolute path outside fixture sandbox`，并给出「改坏识别 → 该测试变红」的证据；
2. **没有引入误报** —— `cargo xtask fixtures verify` 在三平台上对真实的
   5 文件 / 220 条记录仍然通过；
3. sandbox **之内**的单反斜杠路径不被误报。

注意：这只让扫描器多识别一种**候选形态**，**没有**回答「`\` 在 Unix 上算不算分隔符」
那个语义问题。那仍然是 D-B6，留给 R9 真正写路径校验时决定。别顺手把它一起办了。

### 2. ADR-0015 D-1 是我判错了，现在更正

你写的这句我核对了：

> Windows 仍因已登记、未修复的 D-B8 失败，未作任何规避。

**你是对的，而且你上一轮就已经把 Windows runner 的失败原始输出给我了。** 我把 D-B8 标成
「不阻塞」，理由是它不构成泄漏——那个判断没错，但结论错了：它不是安全问题，
却仍然是一条 CI 测试失败。所以 ADR-0015 没有解决问题，只是把阻塞从两个平台扩大到三个。

我在上一轮跟负责人说过：若出现第三次阻塞，就重新评估是否把 recorder 整个移出，
而不是继续一处处让路。现在兑现。

**[ADR-0016](../adr/0016-recorder-out-of-ci-and-single-backslash-roots.md) D-1 取代
ADR-0015 D-1**：`kaleido-recorder` 在**所有平台**退出 CI 测试门禁。

```text
cargo test --workspace --exclude kaleido-recorder
```

排除仍然必须**显式打印**，不允许静默跳过。

**一格都不许减的**（三平台全部保留）：fmt、check-deps、lint-forbidden、
**clippy 含 `spikes/**`**、**fixtures verify**、`crates/**` 与 `xtask` 的全部测试。

另外在 `docs/DEVELOPMENT.md` 里加一条本地检查说明：

```text
cargo test -p kaleido-recorder      # Windows 本机，冻结 spike 的回归保护
```

理由：recorder 已冻结、不会再改，它留下的真正资产是已提交的 fixture，而那些由
`fixtures verify` 在三平台独立校验；`spikes/**` 仍在 clippy 覆盖内所以不会烂掉。
对一段**永不再改**的代码，CI 回归保护的边际价值接近零，而它已经连续三轮阻塞产品主线。

## 三条仍然未修复，不许写成已解决

- **D-B6 / D-B7**（Unix 上 `\` 的语义、macOS `/var` 符号链接别名）：**R9 前置**；
- **D-B8**（`<HOME>` 先于 `<SANDBOX>` 命中）：随 recorder 一起退出 CI，仍未修复，
  R4 脱敏定稿时复查。

## 交付

xtask 的两处改动**分开列出**：排除范围（D-1）与泄漏扫描器识别（D-2）。照旧重建冻结区
哈希基线、证明 Windows 上 `cargo xtask ci` 前后 exit 0。

然后就是原来那三格，一格都不能省：

1. macOS CI 上 Swift 编译成功的原始日志、`swiftc --version`、CI run URL、commit SHA；
2. **「故意改坏 Swift 导出 → CI 那一步真的变红」**的证据；
3. `docs/gates/T-102-evidence.md`，含那句明确结论：
   **「R3 的投影推送能不能走 UniFFI 回调？能／不能，理由是＿＿。」**

最后说一句：三轮阻塞你都定位准确、没有一次用 `allow` / `#[ignore]` / 调顺序绕过，
每次都停下来等裁定。这是对的，继续这样做。这三轮暴露的是仓库从未推送过所积累的债，
不是你的执行问题。
