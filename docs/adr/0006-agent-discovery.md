# ADR-0006: Agent 发现与接入策略（CLI / GUI 双形态）

- 状态：**已接受**（2026-07-28）
- 决策人：项目负责人
- 起草：项目主管
- 触发：负责人实际安装形态说明 + T-004 阻塞报告
- 影响：`docs/REQUIREMENTS.md` §4.2、OBJ-1；`docs/tasks/T-004.md`；未来的 `kaleido-adapter-*`

---

## 背景

负责人本机的实际安装形态：

| Agent | CLI | GUI |
|---|---|---|
| Claude Code | ❌ | ✅ |
| Codex | ✅ | ✅ |
| OpenCode | ✅ | ❌ |

并明确要求：**「你应该同时支持 GUI 和 CLI 版本的协议识别」**。

T-004 首次执行时的推断链是「PATH 上找不到 `claude` ⇒ Claude Code 不可用 ⇒ 整卡阻塞」。
**这个推断是错的**，而且错得很典型 —— 它把「用户装了什么」和「hostd 靠什么起协议进程」混为一谈。

### 核实到的关键事实

`@anthropic-ai/claude-agent-sdk` 通过 npm **optionalDependencies 自带各平台的 Claude Code 原生二进制**
（`@anthropic-ai/claude-agent-sdk-win32-x64` 等）。官方文档原文：

> The SDK bundles a native Claude Code binary for your platform as an optional dependency…
> **You don't need to install Claude Code separately.**

并提供逃生舱 `pathToClaudeCodeExecutable`，可指向用户自己安装的二进制。

**结论：Claude Code 的接入完全不依赖用户是否装了 `claude` CLI。**
`@agentclientprotocol/claude-agent-acp` 经 npm 安装时会把它需要的 Claude Code 二进制一并带来。

---

## 决策

### D-1 把「运行时来源」与「用户安装形态」彻底分开

这是本 ADR 的核心。两件事互相独立：

| 关注点 | 含义 | 受 GUI/CLI 影响吗 |
|---|---|---|
| **运行时来源** | hostd 靠什么把协议进程起起来 | 部分 |
| **会话存储** | A-2「列出并加载历史会话」从哪读（ADR-0003） | **不受影响 —— GUI 与 CLI 共用同一份存储** |
| **认证凭据** | 协议进程用谁的登录态 | **这才是 GUI 安装真正相关的地方** |

用户装 GUI 版不影响我们能不能起进程，它影响的是**凭据落在哪、会话存在哪**。

### D-2 每家的接入策略

| Agent | 运行时来源（按优先级） | 会话存储（A-2） | 认证 |
|---|---|---|---|
| **Claude Code** | 1. hostd 自备 `@agentclientprotocol/claude-agent-acp`（npm，自带二进制）<br>2. 逃生舱：`pathToClaudeCodeExecutable` 指向用户 GUI 内的二进制 | `~/.claude/projects/<encoded-cwd>/*.jsonl`（GUI 与 CLI 共用） | 复用 `~/.claude` 下的登录态。**GUI 登录能否被 bundled 二进制复用，由 T-004 实测确认** |
| **Codex** | 1. PATH（Windows 上是 `codex.cmd`）<br>2. GUI 应用安装目录内置的 codex 可执行 | Codex 的 thread 存储目录（确切路径由 T-004 确认） | 复用 Codex 自身登录态 |
| **OpenCode** | 1. attach 已运行的 server（mDNS 或显式 URL）<br>2. spawn PATH 上的 CLI | 服务端持有 | 服务端持有 |

### D-3 禁止的推断

**「PATH 上没有该 agent 的 CLI」⇒「该 agent 不可用」—— 这个推断一律打回。**

探测失败时，报告必须区分四种情况，不许笼统说「不可发现」：

1. 没装（三种来源都找不到）
2. 装了但解析方式不对（Windows `.cmd` / `PATHEXT`，见 R-6）
3. 装了 GUI 但没装 CLI（**此时多数 agent 仍然可用**）
4. 装了但未登录

### D-4 Node 是硬前置，不是「通常已具备」

`REQUIREMENTS.md §4.2` 原文写「宿主机需有 Node（Claude Code 本身即 npm 分发，通常已具备）」。

**这条在 GUI-only 的机器上不成立** —— Claude Code 桌面版不携带 Node。
Node 应作为**独立的硬前置**处理：hostd 启动时显式探测，缺失时给出确切安装指引。

### D-5 不自动安装第三方包

hostd 检测到 `@agentclientprotocol/claude-agent-acp` 缺失时：

- **不自动执行 `npm install`**
- 给出确切命令（含钉定版本号），由用户确认后再执行

理由：自动向用户机器安装第三方包是有副作用的行为，应当由用户明示同意。

### D-6 UI 呈现的是「怎么检测到的」，不是「可用/不可用」

Agent 列表必须显示每一家的**发现来源**（PATH / GUI 安装目录 / hostd 自备 / 已运行的 server）
与**状态**（就绪 / 未登录 / 缺 Node / 未安装）。缺失时给出可操作的下一步。

一个布尔的「不可用」对用户毫无帮助，也让我们收不到有用的故障报告。

### D-7 平台专属探测收敛在 `platform/`

GUI 应用的安装位置探测是平台专属逻辑，只能出现在 `crates/*/src/platform/{windows,macos,linux}.rs`
（`AGENTS.md §3.5`）。配置目录一律用 `directories` crate，禁止手写 `%APPDATA%` / `~/Library` 字面量。

---

## 被否决的方案

| 方案 | 否决理由 |
|---|---|
| 要求用户为每家 agent 都装 CLI | 与负责人的实际安装形态冲突；对 Claude Code 而言完全没必要 |
| 只支持 CLI 形态，GUI 用户自行安装 CLI | 违反 OBJ-1「无论 CLI 还是 GUI 启动」 |
| hostd 自动 `npm install` 缺失的适配器 | 未经同意向用户机器装包，见 D-5 |
| 从 GUI 应用目录里翻找并直接调用其内部二进制作为**首选**路径 | 依赖未公开的内部布局，上游一次更新即失效。仅作逃生舱 |

---

## 后果

- `REQUIREMENTS.md §4.2` 按 D-4 更新 Node 前置的表述
- T-004 的 Claude Code 录制**不再依赖用户安装 CLI**，阻塞解除
- 未来 `kaleido-adapter-*` 需实现统一的 `AgentDiscovery`，返回结构化的发现结果而非布尔值
- **新增风险 R-11**：GUI 登录态能否被 npm 自备的二进制复用，尚未实测。若不能复用，Claude Code 路径需要用户额外做一次登录 —— T-004 必须给出结论

## 影响的门禁

- **G2**：三家的验收必须覆盖「仅装 GUI」的形态，不许默认 CLI 存在
- **G8**：跨平台冒烟需覆盖 GUI 安装位置探测的三平台差异
