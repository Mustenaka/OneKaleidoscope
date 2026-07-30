# OneKaleidoscope — 需求文档

> 状态：v1 草案，已锁定技术选型。本文件是项目的**需求真源**。
> 任何与本文件冲突的实现都是错误的实现。修改本文件必须走 ADR 流程（见 `CLAUDE.md`）。

**已生效的 ADR 修订**

| ADR | 影响章节 |
|---|---|
| [0001 技术选型固化](adr/0001-technology-selection.md) | §3（mDNS 选型）、§6.2（iroh 术语） |
| [0002 Android 先行](adr/0002-android-first.md) | §8（G4 / G6 互换） |
| [0003 OBJ-1 三级语义](adr/0003-agent-attach-semantics.md) | §1.1 OBJ-1、§4.5、§9（新增 R-9 / R-10） |
| [0004 ACP 版本钉定](adr/0004-acp-version-pinning.md) | §4.2（包名）、§4.5（新增 `Elicitation` 变体）、§9 R-2 |
| [0005 schema 规范化层](adr/0005-schema-normalization-layer.md) | §4.3 / §4.4（生成器选型）、§9 R-4 |
| [0006 Agent 发现策略](adr/0006-agent-discovery.md) | §4.2（Node 前置、GUI/CLI 双形态）、§9（新增 R-11 / R-12） |
| [0007 Elicitation 能力位控制](adr/0007-elicitation-capability-gated.md) | §4.5（取代 ADR-0004 P-5） |
| [0008 版本兼容模型](adr/0008-version-compatibility-model.md) | §4.3 / §4.4、§9 R-4 |
| **[0009 Session Broker](adr/0009-session-broker.md)** | **§1.1 OBJ-1 恢复完整 + 新增 OBJ-7、§4、§8 门禁改端到端场景、新增 §11。取代 ADR-0003 能力模型** |

---

## 0. 一句话定义

把 **Claude Code / Codex / OpenCode** 三家跑在本地的编码 Agent，通过它们各自暴露的**结构化协议**（**不是**终端 PTY 转发），归一化成一套统一协议，经由 **P2P 加密通道**投送到 iOS / Android，实现远程查看项目、驱动会话、审批权限、查看 diff、提交 Git。

---

## 1. 目标与非目标

### 1.1 目标

| # | 目标 | 可验证的含义 |
|---|---|---|
| OBJ-1 | 电脑上跑着 agent 就能远程控制 | **完成一次性集成启用后**，由任意表面（CLI / GUI）创建的会话都进入 hostd 管理的共享运行时；手机可实时查看正在进行的 turn（文本/推理增量、工具、计划、diff）、可审批、可 steer 引导、可取消，原生表面同步反映。启用前已存在的会话可查看历史并恢复。见 [ADR-0009](adr/0009-session-broker.md)<br>**上游确无公开路径的部分进 §11 登记，不得据此降级本条** |
| OBJ-7 | 跨 agent 协作在一个手机端完成 | 同一项目下不同 provider 的会话在手机上归组可见并可切换（例：Claude Code 编排 plan → Codex 执行 → Claude Code 审核）。跨 provider 的**自动化编排引擎**不在 v1，但数据模型不得排除它（[ADR-0009](adr/0009-session-broker.md) D-7） |
| OBJ-2 | 走协议，不走终端 | 代码中不得出现 ANSI 转义解析、屏幕缓冲区抓取来获取 agent 输出 |
| OBJ-3 | 对话内容不经过任何第三方 SaaS | 端到端加密；relay 即使被完全攻陷也拿不到明文 |
| OBJ-4 | PC 端一套代码覆盖三平台 | 单一 Rust workspace，`cargo build` 在 Windows/macOS/Linux 均通过 |
| OBJ-5 | 协议可扩展 | 新增一个 ACP agent 不需要改 proto，只需注册 |
| OBJ-6 | 手机上能完成一次真实编码任务 | 从提问 → 审批工具调用 → 看 diff → commit push，全程只用手机 |

### 1.2 非目标（v1 明确不做，写进代码就是超范围）

- ❌ 云端执行环境（那是 Claude Code on the web 的领域）
- ❌ 完整的移动端代码编辑器（只读 + diff 查看即可）
- ❌ 终端 PTY 转发作为**主路径**（可作为 v2 的 escape hatch）
- ❌ 多用户 / 团队协作 / 权限体系（单人多设备）
- ❌ 自建 LLM 推理、模型路由、成本统计
- ❌ Web 端（v2 再说）

---

## 2. 四个端

### 端 A — `kaleido-hostd`（PC 守护进程，一套代码三平台）

**语言**：Rust（stable，edition 2021 起步）
**形态**：单一可执行文件 + 系统托盘 / 菜单栏图标；支持 `--headless` 无 UI 模式

**职责**
1. Agent 进程监管（spawn / attach / 健康检查 / 崩溃重启 / 优雅退出）
2. 三家 Adapter 的协议归一化 → UACP
3. 会话持久化与**事件日志**（append-only，带单调递增 cursor，支持断线重放）
4. 项目文件树、文件读取、diff 计算（**由 hostd 直接读盘，不经由 agent 的 fs 能力**）
5. Git 操作（status / diff / stage / commit / push）
6. iroh Endpoint（P2P 监听）+ 局域网 mDNS 广播
7. 触发推送（向 relay 发送唤醒信号）

**平台差异清单 —— 必须显式处理，不许「先只做一个平台」**

| 关注点 | Windows | macOS | Linux |
|---|---|---|---|
| 后台常驻 | 托盘程序，可选注册为 Windows Service | 菜单栏 + LaunchAgent | `systemd --user` unit |
| 子进程启动 | `CREATE_NO_WINDOW`；注意 `npx` 实际是 `npx.cmd` | 直接 exec | 直接 exec |
| 路径语义 | 盘符 / UNC / 大小写不敏感 / `\` | 默认大小写不敏感 | 大小写敏感 |
| 文件监听 | ReadDirectoryChangesW | FSEvents | inotify |
| 配置目录 | `%APPDATA%\OneKaleidoscope` | `~/Library/Application Support/OneKaleidoscope` | `$XDG_CONFIG_HOME/onekaleidoscope` |
| 密钥存储 | DPAPI / Credential Manager | Keychain | Secret Service，降级到文件 + 本机密钥 |

统一用 `notify` crate 做文件监听，用 `directories` crate 做路径，**禁止手写平台分支路径逻辑**。

> **Windows 是首要开发与测试平台**（开发者主力环境为 Windows）。macOS/Linux 必须在 CI 中构建并跑单元测试，但门禁的人工验收以 Windows 为准。

---

### 端 B — `kaleido-relay`（验证服务器）

**语言**：Rust + `axum`
**形态**：无状态服务，单个 Docker 容器，可自托管；**也可以完全不部署**（局域网模式下不需要）

**三项职责**

1. **配对验证** — 设备注册、公钥指纹登记、配对码一次性核销
2. **打洞 rendezvous** — 作为 iroh 的 discovery / relay endpoint，帮助 hostd 与手机交换连接信息；直连失败时降级为加密字节流中转
3. **推送转发** — 接收 hostd 的唤醒请求，转发到 APNs / FCM

**零知识约束（硬性，违反即打回）**

- relay 存储的全部内容仅限：设备公钥指纹、推送 token、配对码哈希、最后活跃时间
- **推送载荷只含 `session_id` + 事件类型枚举 + 时间戳。绝不含消息文本、文件路径、代码、工具参数。**
  手机被唤醒后自行通过 iroh 连回 hostd 拉取详情。
- 中转模式下 relay 只见密文，无解密密钥
- relay 不得有任何形式的对话日志

---

### 端 C — iOS

**语言**：Swift 5.9+ / SwiftUI
**最低版本**：iOS 16
**核心复用**：`kaleido-core` 经 UniFFI 打包为 XCFramework
**关键系统能力**：APNs、Keychain、`BGAppRefreshTask`、Face ID / Touch ID 解锁应用

**已知约束**：App 进入后台后 QUIC socket 会被系统回收。因此**推送是唯一可靠的唤醒机制**，App 被唤醒后重新建立 iroh 连接并按 cursor 重放事件。

---

### 端 D — Android

**语言**：Kotlin / Jetpack Compose
**最低版本**：minSdk 26
**核心复用**：`kaleido-core` 经 UniFFI 打包为 AAR（JNI）
**关键系统能力**：FCM、Android Keystore + EncryptedSharedPreferences、ForegroundService（可选常连）、BiometricPrompt

**已知约束**：厂商 ROM 的激进省电策略会杀后台。需引导用户加白名单，并同样以推送为主唤醒路径。

---

## 3. 技术选型（已锁定 — 不得擅自更改，需改走 ADR）

| 层 | 选型 | 理由 |
|---|---|---|
| PC / 核心 | **Rust** | 单静态二进制、低内存常驻、iroh 与 ACP 官方均为 Rust |
| 跨端复用 | **UniFFI** | 协议/传输/会话状态写一次，生成 Swift + Kotlin 绑定，避免三端类型漂移 |
| P2P 传输 | **iroh**（QUIC，v1.0+） | `EndpointId` 即 Ed25519 公钥，天然认证 + E2EE，无需 CA |
| 局域网发现 | **`iroh-mdns-address-lookup`** | 同网段零配置直连；与 iroh Endpoint 直接集成（ADR-0001 C-2） |
| 协议骨架 | **ACP 超集**（自定义为 UACP） | ACP 已定义流式更新、工具调用生命周期、权限请求，正好覆盖移动端渲染需求 |
| 类型真源 | **`kaleido-proto` crate + `schemars`** | 导出 JSON Schema；UniFFI 从同一份类型生成移动端绑定 |
| Relay | **axum** | 轻量、无状态、易容器化 |
| iOS UI | **SwiftUI** | — |
| Android UI | **Jetpack Compose** | — |

---

## 4. Adapter 接入规格

### 4.1 分档

| 档 | 说明 | 覆盖 |
|---|---|---|
| **Tier A（原生协议）** | 直连 agent 自有协议，功能最全：会话列表、历史分页、断线恢复 | Codex、OpenCode |
| **Tier B（ACP 桥）** | 通过 ACP 统一接入，功能取 ACP 交集 | Claude Code + 其余 40 余家 |

Adapter 必须通过 `capabilities()` 显式声明自己支持哪些可选能力，UI 依据声明决定按钮是否可见 —— **禁止在 UI 层硬编码 agent 名称做分支判断**。

~~`AdapterCaps` 必须包含接管能力三级位（ADR-0003）~~ —— **已被 [ADR-0009](adr/0009-session-broker.md) D-3 取代。**

能力位按**具体 runtime 实例**协商（不按 agent 名字、不按版本号）：

```
spawn_owned  resume_persisted  observe_external_live  control_external_live
simultaneous_multi_client  steer_in_flight  approval
queue_visibility  plan_visibility
```

每个会话必须区分 **`history_source`**（历史从哪读）与 **`live_runtime`**（谁在执行、能否订阅与控制）。
原来的 `resume` / `live_attach` 两个布尔位把这两件事压在了一起，导致「能列出 GUI 创建的历史」
被反复误读成「接管了 GUI 正在跑的会话」——该压缩作废。

### 4.2 Claude Code（Tier B）

- 接入方式：hostd spawn `npx @agentclientprotocol/claude-agent-acp` 子进程，走 ACP JSON-RPC over stdio
  （旧包 `@zed-industries/claude-code-acp` 已废弃改名，见 ADR-0004）
- Rust 侧使用官方 `agent-client-protocol` crate，**钉定 1.x（ACP 协议 v1）**，不跟进 v2 Draft
- 版本号必须钉死确切值，不用 `latest` / `^`；升级走 ADR
- **不要**直接对接 `claude --output-format stream-json`：该协议未公开承诺稳定，Zed 的适配器帮你扛住上游变更
- 依赖：**Node 是独立的硬前置**，不可假设「通常已具备」—— Claude Code 桌面版不携带 Node（[ADR-0006](adr/0006-agent-discovery.md) D-4）。hostd 启动时显式探测并在缺失时给出确切安装指引
- **不依赖用户安装 `claude` CLI**：`@agentclientprotocol/claude-agent-acp` 经 npm 安装时会自带各平台的 Claude Code 原生二进制（`@anthropic-ai/claude-agent-sdk-*`）。逃生舱为 `pathToClaudeCodeExecutable`
- **仅装 GUI 的机器同样可用**。GUI 安装真正影响的是认证凭据与会话存储位置，不是能否起协议进程

**文档**
- Agent SDK 总览 — https://platform.claude.com/docs/en/agent-sdk/overview
- CLI reference — https://code.claude.com/docs/en/cli-reference
- Headless 模式 — https://code.claude.com/docs/en/headless
- Hooks — https://code.claude.com/docs/en/hooks
- 官方 Remote Control（竞品对标）— https://code.claude.com/docs/en/remote-control
- ACP 适配器 — https://github.com/zed-industries/claude-agent-acp

### 4.3 Codex（Tier A）

- 接入方式：`codex app-server`，双向 JSON-RPC 2.0
- 传输：v1 用 stdio（本机）；预留 `--listen ws://` + bearer token 以支持 hostd 与 agent 不同机的场景
- **类型不要手写**：用 `codex app-server generate-json-schema` 导出，经规范化层后由生成器产出 Rust 类型，纳入构建流程。
  生成器选型见 [ADR-0005](adr/0005-schema-normalization-layer.md) —— `typify` 已实测无法消化完整 schema（嵌套 `definitions`），最终工具由 T-005 以证据决定
- 核心原语：Thread → Turn → Item

**文档**
- App Server 官方 — https://developers.openai.com/codex/app-server
- codex-rs app-server README — https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md
- CLI — https://developers.openai.com/codex/cli
- ACP 适配器（备选路径）— https://github.com/zed-industries/codex-acp

### 4.4 OpenCode（Tier A）

- 接入方式：`opencode serve --port <p>`，REST + SSE
- **客户端不要手写**：从 `http://localhost:4096/doc` 的 OpenAPI 3.1 spec 生成 Rust 客户端。
  **`progenitor` 只支持 OpenAPI 3.0.x，无法直接消化 3.1 spec**（实测 `exclusiveMinimum` 数值/布尔冲突）。
  最终工具由 T-005 以证据决定，见 [ADR-0005](adr/0005-schema-normalization-layer.md)
- 自带 mDNS 局域网发现，可复用

**文档**
- Server — https://opencode.ai/docs/server/
- SDK — https://opencode.ai/docs/sdk/
- ACP — https://opencode.ai/docs/acp/
- CLI — https://opencode.ai/docs/cli/
- Repo — https://github.com/sst/opencode

### 4.5 统一 Trait（合同）

```rust
#[async_trait]
pub trait AgentAdapter: Send + Sync {
    fn id(&self) -> AdapterId;
    fn capabilities(&self) -> AdapterCaps;

    async fn spawn(&self, cwd: &Path, cfg: &AgentConfig) -> Result<SessionHandle>;
    async fn attach(&self, id: &SessionId) -> Result<SessionHandle>;
    async fn list_sessions(&self) -> Result<Vec<SessionSummary>>;   // 由 caps.resume 控制，三家均需支持（ADR-0003 A-2）
    async fn prompt(&self, id: &SessionId, blocks: Vec<ContentBlock>) -> Result<TurnId>;
    async fn cancel(&self, id: &SessionId) -> Result<()>;
    async fn shutdown(&self, id: &SessionId) -> Result<()>;

    /// 归一化事件流
    fn events(&self, id: &SessionId) -> BoxStream<'static, SessionEvent>;

    async fn respond_permission(&self, req: PermissionId, d: Decision) -> Result<()>;
}
```

`SessionEvent` 至少必须覆盖以下 **12 个**变体：
`MessageChunk` / `ThoughtChunk` / `ToolCallStart` / `ToolCallUpdate` / `ToolCallEnd` /
`PermissionRequest` / `Elicitation` / `PlanUpdate` / `DiffProduced` / `TurnStart` / `TurnEnd` / `Error`

> `Elicitation`（agent 要求结构化表单输入）由 ADR-0004 P-5 加入，
> 经 [ADR-0007](adr/0007-elicitation-capability-gated.md) 修正为**由 `caps.elicitation` 能力位控制**：
> Codex 侧对应 `mcpServer/elicitation/request`；**ACP v1 不存在 elicitation**（实测
> `grep -i "elicit" schemas/acp/schema.json` 无命中），故 Claude Code 在 v1 内不支持。
> 其余 11 个变体仍要求三家全覆盖。
>
> 参考：ACP v1 的 `SessionUpdate` 实际变体为
> `user_message_chunk` / `agent_message_chunk` / `agent_thought_chunk` / `tool_call` /
> `tool_call_update` / `plan` / `available_commands_update` / `current_mode_update` /
> `config_option_update` / `session_info_update` / `usage_update`。
>
> **判别字符串使用 UACP 自己的取值，不复用 ACP 的字符串**（ADR-0004 P-3），
> adapter 层负责映射，使上游改名只影响单个 adapter crate。

---

## 5. UACP 协议要求

协议规范文件 `docs/PROTOCOL.md` 与 `crates/kaleido-proto` **由项目主管产出**，必须满足：

- JSON-RPC 2.0 语义，双向（hostd 也能主动请求手机，例如权限审批）
- 全部类型在 `kaleido-proto` 中用 Rust struct/enum 定义，`serde` + `schemars` 派生
- 事件流带**单调递增 cursor**，客户端断线后用 `since: cursor` 重放，保证不丢不重
- 请求 ID 空间：客户端发起与服务端发起分离
- 版本协商：握手时交换 `protocol_version`，不兼容时明确报错而非静默降级
- 能力协商：hostd 声明可用 adapter 与各自 caps，客户端据此渲染

必须覆盖的方法族：
`hello` / `session.*` / `prompt.*` / `permission.*` / `fs.*` / `git.*` / `push.*` / `event.replay`

---

## 6. 传输与安全模型

### 6.1 连接分层（按优先级降级）

| 层 | 机制 | 是否需要 relay |
|---|---|---|
| **L0** | 局域网 mDNS 发现 + iroh 直连 | ❌ 完全不需要 |
| **L1** | iroh NAT 打洞（QUIC hole punching） | ⚠️ 仅需 rendezvous 交换连接信息 |
| **L2** | iroh relay 加密中转 | ✅ 需要，但零知识 |
| **L3** | 用户自备隧道（Tailscale / Cloudflare Tunnel） | 由用户自理 |

客户端必须自动按 L0 → L1 → L2 尝试，并在 UI 上**明示当前处于哪一层**。

### 6.2 配对

- hostd 生成二维码，内容为 `EndpointId(Ed25519 公钥) + 一次性配对码 + 可选 relay 地址`
- 手机扫码 → 双向验证 → 各自把对方公钥写入本地安全存储
- **不需要 CA、不需要证书 pinning**：iroh 的 `EndpointId` 本身就是公钥

> iroh 1.0 已将 `NodeId` / `NodeAddr` 更名为 `EndpointId` / `EndpointAddr`，`Connection::conn_type()` 被
> `Connection::paths()` + `Path::is_ip()` / `is_relay()` / `is_selected()` 取代。见 ADR-0001 C-1。
- 配对码一次性、有效期 5 分钟
- 支持在 hostd 上吊销单个设备

### 6.3 硬性安全要求

- 私钥只存在于设备安全存储（Keychain / Keystore / DPAPI），**不得落到普通文件或日志**
- 日志中禁止出现：文件内容、工具调用参数明文、密钥、token
- 推送载荷不含任何业务内容（见 §2 端 B）
- 权限审批的决定必须由客户端私钥签名，hostd 验签后才执行
- hostd **绝不监听 `0.0.0.0` 的明文端口**

---

## 7. v1 功能范围

### 7.1 必做（Must）

| # | 功能 | 验收标准 |
|---|---|---|
| F-1 | 流式对话 | 手机上逐字看到 agent 回复，延迟 < 300ms（局域网） |
| F-2 | Tool call 卡片 | 每次工具调用显示名称、参数摘要、状态、结果折叠区 |
| F-3 | 权限审批 + 推送 | 收到推送 → 点开 → 一键批准/拒绝 → agent 继续；全程 < 10s |
| F-4 | 文件树浏览 | 可浏览项目任意文件，支持搜索，尊重 `.gitignore` |
| F-5 | Diff 查看 | 语法高亮的行级 diff，支持按文件切换 |
| F-6 | Git 操作 | status / stage / commit / push，含冲突时的明确报错 |
| F-7 | 会话管理 | 列表、新建、恢复、重命名、删除；标注所属 adapter 与项目 |
| F-8 | 断线恢复 | 手机飞行模式 30s 后恢复，事件不丢不重 |
| F-9 | 三家 adapter 全通 | 每家都能完成 F-1~F-3 |
| F-10 | 二维码配对 | 30 秒内完成首次配对 |

### 7.2 不做（v1）

图片/文件上传、语音输入、多 worktree 并行、终端 escape hatch、Web 端、会话分享。

---

## 8. 里程碑与门禁

> 门禁分两类：**【人工】**= 开发者本人真机测试并签字；**【审核】**= 项目主管代码审查。
> 未通过门禁不得进入下一阶段。这是防止实现偏离需求的唯一手段。

| 门禁 | 阶段产出 | 通过条件 | 类型 |
|---|---|---|---|
| **G0** | iroh 打洞可行性 spike | Rust demo 在「家宽 hostd ↔ 4G 手机」下测 20 次，记录直连成功率。<br>**< 60% 则必须把 L2 relay 提升为 v1 必做项** | 【人工】 |
| **G1** | `PROTOCOL.md` + `kaleido-proto` 定稿 | 主管确认覆盖 §5 全部方法族；JSON Schema 可导出；**用真实类型生成 Swift 与 Kotlin 绑定并双双编译通过**（ADR-0001 D-6 / ADR-0002 D-2，前置缓解 R-5） | 【审核】 |
| **G2** | hostd + Rust CLI 客户端走通 | 用 CLI（先不碰手机）驱动**三家** agent 各完成一次含工具调用与权限审批的完整 turn；另对每家做一次「外部 CLI/GUI 创建会话 → hostd 列出并加载 → 继续对话」（ADR-0003 A-2）与一次 R-9 冲突观测 | 【人工】 |
| **G3** | 传输层 + 配对 + 事件重放 | 扫码配对成功；拔网 30s 重连后事件序列与未断线时逐字节一致 | 【人工】 |
| **G4** | **Android** App（F-1~F-3, F-7） | 仅用 Android 手机完成一次真实编码任务并合入（ADR-0002） | 【人工】 |
| **G5** | fs / git / diff 全量（F-4~F-6） | 在 Android 手机上看 diff 并 commit push 成功 | 【人工】 |
| **G6** | **iOS** 对齐 | 功能与 Android 一致，通过 F-1~F-10（ADR-0002） | 【人工】 |
| **G7** | 安全审查 | 主管逐条核对 §6.3；日志抽样确认无敏感信息泄漏 | 【审核】 |
| **G8** | 跨平台构建 | Windows/macOS/Linux CI 全绿；三平台各手动冒烟一次 | 【人工】 |

---

## 9. 风险登记

| ID | 风险 | 影响 | 缓解 |
|---|---|---|---|
| R-1 | iroh 在运营商级 NAT / 对称 NAT 下打洞失败率高 | 核心体验不可用 | G0 提前验证；保留 L2 relay 与 L3 隧道 |
| R-2 | **ACP 规范自身处于 v1→v2 过渡**（crate 已发 2.0.0 而协议 v2 仍为 Draft，`session/update` 判别字符串已改名），且 Claude Code 适配器迭代频繁 | UACP 事件语义漂移；adapter 编译失败；Claude Code 功能缺失 | ACP 与适配器双钉定（ADR-0004 P-1/P-2）；UACP 判别值与 ACP 解耦（P-3）；schema 快照 + CI 每日 diff（P-4）；capabilities 优雅降级；预留直连 stream-json 的 plan B |
| R-3 | iOS/Android 后台被杀导致连接不可靠 | 推送不及时 | 推送为唯一唤醒真源；App 冷启后按 cursor 重放 |
| R-4 | **【已发生，且变更速率极高】**T-006 实测 Codex `0.144.6 → 0.146.0` 一次小版本升级产生 **2380 处语义漂移**（added=548 / changed=1598 / removed=234）。此外上游 schema 无法被指定生成器消化：progenitor 只支持 OpenAPI 3.0.x 而 OpenCode 输出 3.1.0；typify 无法解析 Codex schema 的嵌套 `definitions`。此外仍存在 Codex / OpenCode 协议 breaking change 的持续风险 | 自动生成链在写第一行 adapter 前即断裂；adapter 编译失败 | 引入受纪律约束的**规范化层**（[ADR-0005](adr/0005-schema-normalization-layer.md)）；生成器选型改为以证据决定（T-005）；`schemas/` 原样快照 + 每日语义 diff 监控漂移（T-003） |
| R-5 | UniFFI 类型表达能力受限（泛型、async 流） | 核心 API 被迫妥协 | 在 G1 阶段就用真实类型验证绑定生成，不要等到 G4 |
| R-6 | Windows 上 npx/Node 子进程管理坑多 | Claude Code adapter 不稳 | Windows 作为首要测试平台；显式处理 `.cmd` 与进程树终止 |
| R-7 | 需求被 AI 实现方悄悄偏离 | 交付物不符预期 | 门禁 + 契约测试 + 主管审核；`kaleido-proto` 为不可擅改的合同 |
| R-8 | 三家的权限模型不同构：ACP 是「agent 提供动态选项数组 + 客户端回 optionId」，Codex 是两个分开的服务端请求 + 固定字符串枚举 | 无法无损归一化，UACP 设计一旦选错方向需推倒重来 | UACP 采用「选项列表 + 选项 id」形状（表达力更强的一侧），由 Codex adapter 合成固定选项集；G1 定稿前必须用三家真实录制 fixture 验证一遍 |
| R-9 | hostd 与用户的 CLI/GUI 并发读写同一份会话存储（`~/.claude/projects`、Codex thread 目录） | 会话损坏或事件错乱 | v1 内加载即独占，UI 明示「已被手机接管」；G2 必须实测并把实际行为记入 `docs/gates/G2-result.md`，不许假设安全（ADR-0003） |
| R-10 | `loadSession` / `sessionCapabilities` 是能力位而非保证，适配器升级可能撤销 | 会话列表功能突然消失 | 握手时检查能力位，为 false 时降级为「只能新建」并在 UI 明示原因；禁止崩溃或静默隐藏（ADR-0003） |
| R-11 | GUI 写入的登录态能否被 npm 自备的 Claude Code 二进制复用，尚未实测 | 若不能复用，Claude Code 路径需要用户额外登录一次，影响 OBJ-1 的开箱体验 | T-004 必须实测并给出结论；不能复用时在 UI 明示并给出可操作指引（ADR-0006） |
| R-13 | **【已发生】**同一个 agent 在一台机器上存在**多个二进制实例**（Store/GUI 版、npm CLI 版、npx 临时版），**各自持有独立的登录态**。T-006 实测：`where.exe codex` 命中 Store GUI 版并返回 `Not logged in`，而用户终端里的 CLI 版返回 `Logged in using ChatGPT` | hostd 找到了 agent 却用不了；用户看到「已登录」但 hostd 报未登录，无法自查 | 发现结果必须**按候选逐个报告登录态**，优先选择已认证的候选；UI 呈现「选中了哪一个二进制、它的登录态如何」，并允许用户显式指定（[ADR-0006](adr/0006-agent-discovery.md) 优先级①） |
| R-12 | **【已发生】**hostd 以托盘 / LaunchAgent / systemd user unit 启动时**不执行用户的 shell profile**，拿不到 conda / nvm / profile 注入的 PATH。用户在终端里跑得好好的 agent，hostd 找不到 | 三家 agent 全部「未安装」，OBJ-1 直接失效 | 多源发现：显式配置 → 继承 PATH → 平台持久化环境变量 → 已知安装位置 → hostd 自备；失败时报告「在 5 处分别看到什么」而非「未安装」（[ADR-0006](adr/0006-agent-discovery.md) D-8） |

---

## 10. 目录结构（约定）

```
OneKaleidoscope/
├── CLAUDE.md                    # 项目主管角色定义
├── AGENTS.md                    # 落地实现方规范
├── docs/
│   ├── REQUIREMENTS.md          # 本文件（需求真源）
│   ├── PROTOCOL.md              # ← 主管产出
│   ├── ARCHITECTURE.md          # ← 主管产出
│   ├── MILESTONES.md            # ← 主管产出（门禁拆解为可执行任务）
│   └── adr/                     # 架构决策记录
├── crates/
│   ├── kaleido-proto/
│   ├── kaleido-transport/
│   ├── kaleido-core/            # → UniFFI
│   ├── kaleido-hostd/
│   ├── kaleido-cli/             # G2 用的验证客户端
│   ├── kaleido-adapter/
│   ├── kaleido-adapter-acp/
│   ├── kaleido-adapter-codex/
│   ├── kaleido-adapter-opencode/
│   └── kaleido-relay/
├── ios/
├── android/
└── .github/workflows/
```

---

## 11. 上游阻塞登记（[ADR-0009](adr/0009-session-broker.md) D-6）

> **本表的存在意义是：需求永不因实现困难而降级。**
> 上游确实没有公开路径的能力登记在此，需求条目保持原样。
> **门禁不得因为登记了阻塞就算通过**，只能标注「该项受 UB-N 阻塞」。

| ID | 缺失能力 | 影响的需求 | 已验证到什么程度 | 当前替代 | 复查触发条件 |
|---|---|---|---|---|---|
| **UB-1** | Codex Desktop（GUI）是否连接共享 app-server daemon 未知。若为私有实例，则无法订阅 Desktop 正在进行的 turn | OBJ-1 的 GUI × `observe_external_live` / `control_external_live` | `codex app-server` **已确认**提供 `daemon` 子命令、`proxy`（连接*正在运行的* control socket）、`--listen unix://\|ws://`、capability-token/JWT 鉴权。Desktop 侧行为**未实测** | Broker 覆盖 CLI；Desktop 会话可 `resume_persisted` | T-013 spike 结论；Codex 版本更新 |
| **UB-2** | Claude Code 官方 `/remote-control` 的第三方客户端协议未公开，对端是厂商服务端 | OBJ-1 中「既存 Claude GUI/CLI 会话」的实时接管 | 官方文档确认该功能存在且面向 Anthropic 自有 Web/App | hostd 经 Agent SDK / `claude-agent-acp` 拥有的会话可完整实时（`spawn_owned` + `simultaneous_multi_client`） | 上游公开第三方接入协议 |
| **UB-3** | Codex 快照中不存在 `subagent/*`（0 命中），无法表达 Codex 侧子 agent 树 | 跨 agent 编排（OBJ-7）的细粒度呈现 | 已对 `schemas/codex/` 0.146.0 逐条 grep | 用 `turn/plan/updated` 表达任务/计划层级 | Codex 新增该方法族 |
| **UB-4** | ACP v1 无 elicitation（0 命中） | `Elicitation` 变体在 Claude Code 路径不可用 | 已对 `schemas/acp/schema.json` grep 验证，见 [ADR-0007](adr/0007-elicitation-capability-gated.md) | `caps.elicitation` 能力位关闭 | ACP v2 转 stable |

### 登记纪律

1. 进入本表前**必须有实测证据**，不许凭文档推断（教训见 ADR-0007 §主管的自我复盘）
2. 每条必须写明**复查触发条件**，不许无限期挂着
3. 上游一旦补齐，**立即销号并恢复对应门禁格**
