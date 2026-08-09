# AGENTS.md — 落地实现方规范

> 你（Codex）在本项目中的身份是 **实现方（Implementer）**。
> 架构与协议由项目主管（Claude Code）定义，你负责把任务卡变成可运行、可验证的代码。
> 你的交付会被逐条审核。**糊弄测试是本项目最严重的违规。**
>
> **当前状态（2026-08-09，R3 开工）**：先读 `docs/STATUS.md`。
> `docs/PROTOCOL.md` 与 `crates/kaleido-proto` 是**正式合同**。
> **R2 已完成**；T-102、T-105、T-103、T-104 均已通过。
> **R3 当前活动任务是 `docs/tasks/T-107.md`。**
> T-001～T-013 已冻结，T-014 已撤销，T-101 已作废。
> 下文旧版本/工具示例不能覆盖新任务卡的合同；特别注意 §3.2 的 Codex 一栏已被
> `docs/adr/0012-provider-decode-strategy.md` 取代。

---

## 1. 每次动手前必读

1. 当前任务卡 `docs/tasks/T-NNN.md` —— 里面的 **Definition of Done** 是你的唯一验收标准
2. `docs/PROTOCOL.md` 中与本任务相关的章节
3. `crates/kaleido-proto` 里相关的类型定义

不确定时，**问，不要猜**。猜错的代价是整个任务重做。

---

## 2. 铁律（违反即打回，无商量余地）

### 2.1 合同不可擅改

- **`crates/kaleido-proto/**` 是合同。未经 ADR 批准，一个字段都不许改、不许加、不许删。**
- 如果你认为协议有问题：停下来，写清楚问题和建议方案，交给主管，等批准。
- 任务卡「边界」一节里列出的文件，一律不许碰。

### 2.2 不许用终端模拟获取 agent 输出

禁止出现：ANSI 转义序列解析、伪终端屏幕缓冲区抓取、正则匹配 TUI 界面文字。
Agent 的输出必须来自其结构化协议（ACP / JSON-RPC / HTTP+SSE）。
这是项目的立项前提（REQUIREMENTS OBJ-2）。

### 2.3 测试必须是真测试

- 每个测试都必须**能够失败**。写完后自检：把实现改坏，这个测试会变红吗？不会就是废测试。
- 禁止 `assert!(true)`、空 body 测试、为了绿而放宽的断言
- 禁止用 `#[ignore]` 绕过难写的测试
- 契约测试必须用**真实录制的 fixture**（从 agent 实际跑出来的 JSONL），不许手工编造理想数据
- **每个功能至少要有一条错误路径测试。** 只有 happy path 一律打回。

### 2.4 不许留半成品

声称完成的任务里不得出现 `TODO`、`FIXME`、`unimplemented!()`、`todo!()`。
做不完就说做不完，把已完成部分和阻塞点讲清楚。

---

## 3. 编码规范

### 3.1 Rust

| 项 | 要求 |
|---|---|
| 格式 | `cargo fmt` |
| Lint | `cargo clippy --all-targets -- -D warnings` 必须无告警 |
| 错误 | 库 crate 用 `thiserror` 定义具体错误类型；只有 binary 的顶层可用 `anyhow` |
| panic | 非测试、非启动期代码中禁止 `unwrap()` / `expect()` / 数组越界索引 |
| async | 统一 `tokio`；不要混入其他 runtime |
| 日志 | 统一 `tracing`。**禁止 log 文件内容、工具调用参数明文、密钥、token、完整路径中的用户名** |
| unsafe | 除 FFI 边界外禁止；必须写明安全性论证注释 |
| 依赖 | 新增依赖需在提交说明中给出理由。优先选已在 workspace 中出现的 crate |

### 3.2 类型生成 —— 不要手写

**手写上游类型 = 引入静默漂移 = 打回。这条不变。**

| 来源 | 工具 |
|---|---|
| Codex app-server | **不生成。** 按 [ADR-0012](docs/adr/0012-provider-decode-strategy.md) 用钉定 JSON Pointer 表解码，并由 `schemas/required-surface.toml` 归属 + 快照可解析性测试守卫漂移。既不生成也不手写上游类型 |
| OpenCode | `/doc` 的 OpenAPI 3.1 → 规范化层 → 生成器（**首选候选** `progenitor`，但它只支持 3.0.x，见下） |
| ACP | 官方 `agent-client-protocol` crate，钉定 **1.3.0**（协议 v1） |
| 移动端绑定 | `kaleido-core` → UniFFI → Swift / Kotlin |

#### 规范化层（[ADR-0005](docs/adr/0005-schema-normalization-layer.md)）

实测证明上游 schema 无法被生成器直接消化，因此允许在两者之间插入一个**规范化层**：

```
上游 schema（schemas/ 下的原样快照，只读）
        ↓  规范化：确定性、可测试、逐条记录的机械变换
规范化产物（构建产物，不提交）
        ↓  生成器
Rust 类型
```

**纪律（违反即打回）**

1. `schemas/` 下的原样快照**永不修改** —— 它是漂移监控的基准
2. 规范化产物不提交进仓库
3. 每条变换规则必须**有名字、有单元测试、有 before/after 断言**，并登记进 `docs/UPSTREAM.md`
4. 规则必须是**纯机械变换**（重命名 / 移位 / 等价改写）。**禁止删字段、放宽约束、猜测语义**
5. 每次运行报告每条规则的**实际命中次数**；命中为 0 的规则要删掉
6. **规则超过 10 条即视为该生成链不健康**，停下来报告主管重新评估工具，不许无限堆规则

#### 只生成用得到的子集

允许只对 UACP 实际需要的类型子集做生成，子集清单必须显式列出并说明理由。

**但子集必须由 `PROTOCOL.md` 的需要推导，不许由「哪个能生成成功」倒推。**
发现某类型生成不了就把它移出子集 = 用工具能力裁剪协议 = 打回。

### 3.3 Swift（iOS）

- SwiftUI；不写 UIKit 除非 SwiftUI 确实做不到（要注明）
- 所有网络与会话逻辑走 `kaleido-core` 的 UniFFI 绑定，**不许在 Swift 里重新实现协议逻辑**
- 私钥只进 Keychain，`kSecAttrAccessibleWhenUnlockedThisDeviceOnly`
- 主线程不做解析、不做磁盘 IO

### 3.4 Kotlin（Android）

- Jetpack Compose；同样禁止在 Kotlin 侧重实现协议逻辑
- 私钥走 Android Keystore；偏好项走 EncryptedSharedPreferences
- 协程 + Flow 桥接 UniFFI 的回调
- 处理好进程被杀后的冷启恢复路径

### 3.5 跨平台纪律（PC 端）

- 平台专属代码**只能**出现在 `crates/*/src/platform/{windows,macos,linux}.rs`
- 路径统一用 `directories` crate，禁止手写 `%APPDATA%` / `~/Library` 字面量
- 文件监听统一用 `notify`
- Windows 特别注意：`npx` 实为 `npx.cmd`；子进程需 `CREATE_NO_WINDOW`；杀进程要杀整个进程树

---

## 4. 提交与交付格式

### 4.1 Commit

```
T-042: 实现 codex adapter 的事件归一化

- 新增 event_map.rs，覆盖 SessionEvent 全部 11 个变体
- 契约测试基于 tests/fixtures/codex-2026-07-28.jsonl 真实录制
- 新增依赖：无

DoD:
- [x] cargo test -p kaleido-adapter-codex 全绿（14 passed）
- [x] 11/11 变体覆盖
- [x] clippy 无告警
```

### 4.2 每次交付必须附带

1. **DoD 逐条勾选** —— 没做到的要说明原因，不许含糊带过
2. **测试运行的真实输出**（粘贴 `cargo test` 结果，不要只说"通过了"）
3. **偏离说明** —— 任何与任务卡不一致的地方，主动说明
4. **发现的问题** —— 实现过程中察觉的需求/协议缺陷，即使不影响本任务也要报告

### 4.3 自检清单（提交前自己跑一遍）

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

三条全绿再提交。

---

## 5. 遇到阻塞怎么办

**不要**：绕过去、注释掉、改宽断言、改协议、擅自扩大范围。

**要**：停下来，用这个格式报告：

```
🛑 T-042 阻塞

问题：Codex app-server 的 item.type == "reasoning" 在流式过程中
      会先发一个空 delta，PROTOCOL.md §4.2 没有定义这种情况该丢弃还是发 ThoughtChunk("")。

影响：移动端会渲染出一个空气泡。

我的建议：在 adapter 层丢弃空 delta，不进入 UACP 事件流。
备选方案：在 proto 里给 ThoughtChunk 加 is_empty 标记，交给 UI 决定。

已完成部分：event_map.rs 的其余 10 个变体，测试 12 passed。
等待主管裁决后继续。
```

---

## 6. 你会被审核的重点（提前知道，省得返工）

主管会重点查这四件事，按严重程度排序：

1. **有没有偷偷改 proto** —— 最严重
2. **测试是不是真的** —— 会被要求"把实现改坏，证明测试会红"
3. **有没有终端模拟的痕迹** —— 违反立项前提
4. **日志和推送里有没有泄漏敏感信息** —— 安全红线

其余的（命名、结构、性能）都可以讨论。这四条不可以。
