# AGENTS.md — 编码规范

> 生效：2026-08-09。**本文不再区分「主管」和「实现方」。**
> 进入这个仓库的任何人（Claude Code、Codex、人）都是同一个角色：读目标、挑下一项、
> 自己设计、自己实现、自己验证、把结论写下来。
>
> **工作方式看 [CLAUDE.md](CLAUDE.md)**：目标、当前进度、默认直接写、五条硬约束、
> 协议改法、何时才停下来问。本文只讲技术约定。
>
> 旧版本里「等批准」「一次只接一张卡」「阻塞报告模板」「你的交付会被逐条审核」
> 已全部作废。下文的工具/版本示例若与当前代码冲突，以代码和 ADR 为准。

---

## 1. Rust

| 项 | 要求 |
|---|---|
| 格式 | `cargo fmt --all` |
| Lint | `cargo clippy --all-targets -- -D warnings` 无告警 |
| 错误 | 库 crate 用 `thiserror` 定义具体错误类型；只有 binary 顶层可用 `anyhow` |
| panic | 非测试、非启动期代码禁止 `unwrap()` / `expect()` / 越界索引（workspace lint 已 deny） |
| async | 统一 `tokio` |
| 日志 | 统一 `tracing`。禁止打印文件内容、工具调用参数明文、密钥、token、含用户名的完整路径 |
| unsafe | 除 FFI 边界外禁止；必须写安全性论证注释 |
| 依赖 | 新增依赖在提交说明里给理由；优先用 workspace 里已有的 crate |
| 半成品 | 声称完成的代码里不留 `TODO` / `FIXME` / `todo!()` / `unimplemented!()`。做不完就说做不完 |

新增 crate 时要同步三处：workspace `members`、`docs/dependency-rules.toml` 的
`[crates."<name>"]` 条目、该条目的 `may_depend_on` 白名单。
`cargo xtask check-deps` 会挡住漏项。细节见 [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md)。

## 2. 测试

- **每个测试都要能失败。** 写完自检：把实现改坏，它会变红吗？不会就是废测试。
  改协议或改核心语义时，实际做一次这个变异验证，把结果写进提交说明。
- 禁止 `assert!(true)`、空 body、为了绿而放宽断言、用 `#[ignore]` 绕过难写的测试。
- 契约测试用**真实录制的 fixture**（Agent 实际跑出来的 JSONL），不手工编造理想数据。
  需要新语义证据时补录，不必先收齐所有 provider 的所有事件。
- **每个功能至少一条错误/拒绝路径测试。** 只有 happy path 的功能算没做完。
- 测试代码可以在 `tests/` 文件顶部或 `#[cfg(test)]` 模块内 allow
  `clippy::unwrap_used` 与 `clippy::expect_used`，仅此两项，且不放在 crate 根。

## 3. 上游类型：不要手写

**手写上游类型会引入静默漂移。** 各来源的处理方式：

| 来源 | 做法 |
|---|---|
| Codex app-server | **不生成也不手写。** 按 [ADR-0012](docs/adr/0012-provider-decode-strategy.md) 用钉定 JSON Pointer 表解码，`schemas/required-surface.toml` 归属 + 快照可解析性测试守卫漂移 |
| OpenCode | `/doc` 的 OpenAPI 3.1 → 规范化层 → 生成器 |
| ACP | 官方 `agent-client-protocol` crate，钉定 1.3.0（协议 v1） |
| 移动端绑定 | `kaleido-core` → UniFFI（钉定 `=0.32.0`）→ Swift / Kotlin |

规范化层（[ADR-0005](docs/adr/0005-schema-normalization-layer.md)）的纪律：
`schemas/` 下原样快照永不修改（它是漂移基准）；规范化产物不提交；
每条规则有名字、有单元测试、有 before/after 断言并登记进
[docs/UPSTREAM.md](docs/UPSTREAM.md)；规则必须是纯机械变换，不删字段、不放宽约束、
不猜语义；命中为 0 的规则删掉；规则超过 10 条说明这条生成链不健康，换工具而不是堆规则。

只对当前纵切需要的类型子集做生成，但**子集由 `PROTOCOL.md` 的需要推导**，
不能由「哪个生成得动」倒推。

## 4. 平台

### Swift（iOS）

- SwiftUI；用 UIKit 要注明原因。
- 网络与会话逻辑走 `kaleido-core` 的 UniFFI 绑定，**不在 Swift 里重实现协议逻辑**。
- 私钥只进 Keychain，`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`。
- 主线程不解析、不做磁盘 IO。

### Kotlin（Android）

- Jetpack Compose；同样不在 Kotlin 侧重实现协议逻辑。
- 私钥走 Android Keystore，偏好项走 EncryptedSharedPreferences。
- 协程 + Flow 桥接 UniFFI 回调。
- 处理进程被杀后的冷启恢复。

### PC 跨平台

- 平台专属代码只放 `crates/*/src/platform/{windows,macos,linux}.rs`，
  **写缺省分支**——`android` 不等于 `linux`（这条踩过，见 D-B4）。
- 路径用 `directories` crate，不手写 `%APPDATA%` / `~/Library` 字面量。
- 文件监听用 `notify`。
- Windows：`npx` 实为 `npx.cmd`；子进程加 `CREATE_NO_WINDOW`；杀进程要杀整棵进程树。

## 5. 提交

格式和内容要求见 [CLAUDE.md](CLAUDE.md) §7。提交前跑：

```bash
cargo xtask ci
```

它是唯一的本地入口，按序跑 fmt、依赖守卫、反模式扫描、clippy、测试、fixture 校验，
首个失败即停。
