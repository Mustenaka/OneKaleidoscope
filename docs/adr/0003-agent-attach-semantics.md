# ADR-0003: OBJ-1「接管」的三级语义

- 状态：**已被取代**（2026-07-30）—— 能力模型由 [ADR-0009](0009-session-broker.md) D-2/D-3 取代
- 保留价值：本文档的**一手核实证据**（三家的会话存储位置、`session/load` 会重放历史、
  `claude-agent-acp` 与 CLI/GUI 共用 `~/.claude/projects`）仍然有效，继续引用
- 作废部分：`A-1 create / A-2 resume / A-3 live_attach` 三位制。它把
  「能列出历史」与「能接管正在跑的 turn」压成了同一维度，并据此**软化了 OBJ-1** ——
  那是拿需求迁就实现，ADR-0009 §1 已明确认定为本项目至今最严重的管理错误
- 原状态：已接受（2026-07-28）
- 决策人：项目负责人
- 起草：项目主管
- 影响：`docs/REQUIREMENTS.md` §1.1 OBJ-1、§4.5 `AgentAdapter` trait、`docs/PROTOCOL.md` 能力协商

---

## 背景

`REQUIREMENTS.md` OBJ-1 的可验证含义原文是：

> 无论 CLI 还是 GUI 启动，hostd 都能**接管或新建**会话

项目负责人明确要求：**Claude Code、Codex、OpenCode 三家都要支持，且包括 GUI 与 CLI 两种启动方式。**

问题在于「接管」是一个含混的词。核对三家官方协议后发现，它至少对应三种能力完全不同的行为，而三家 agent 对这三种行为的支持度不一致。不把它拆开，实现方会各自理解、UI 会做出无法兑现的入口。

### 核对结论（这一节是本 ADR 的事实依据）

| Agent | 会话存储 | 列出 | 加载历史 | 实时接管进行中的会话 |
|---|---|---|---|---|
| **Codex** | 磁盘持久化 thread | `thread/list`（分页）、`thread/loaded/list` | `thread/resume` | ❌ 另一个 app-server 进程正在跑的 turn，我们看不到 |
| **Claude Code**（ACP 桥） | `~/.claude/projects/<encoded-cwd>/<sid>.jsonl`，**与 CLI / GUI 共用同一个存储** | ACP `session/list` | ACP `session/load`（受 `loadSession` 能力位控制，适配器已支持） | ❌ 同上 |
| **OpenCode** | 服务端持有 | `GET /session` | `GET /session/:id/message` | ✅ `opencode serve` 是常驻服务，`opencode attach <url>` + `GET /event` SSE 天然支持多客户端 |

关键事实：**ACP 的 `session/load` 会把整段历史以 `session/update` 通知的形式重放给客户端**，直到全部条目发完才响应原始请求。这意味着「加载历史会话」这件事天然产出我们需要的事件流，不需要额外的历史查询 API。

> 更正：我在前一轮判断「Tier B 只能新建、无法接管 GUI/CLI 创建的会话」是错的。`claude-agent-acp` 与 Claude Code CLI/GUI 共用 `~/.claude/projects` 存储并支持 `loadSession`，因此 Tier B 可以列出并加载由 GUI/CLI 创建的历史会话。真正做不到的只有「实时接管另一个前端正在跑的 turn」。

---

## 决策

把「接管」拆成三级能力，逐级由 `capabilities()` 声明，UI 按声明渲染。

### A-1 `create` —— 新建会话

**三家均必须支持。v1 硬性要求。**

hostd 自行 spawn / 连接 agent，创建全新会话。

### A-2 `resume` —— 列出并加载由任意前端创建的历史会话

**三家均必须支持。v1 硬性要求 —— 这是「包括 GUI 和 CLI」的落地含义。**

无论会话是由 CLI、GUI 桌面端还是 IDE 插件创建，只要它落进了该 agent 的会话存储，hostd 就必须能：

1. 列出它（含项目路径、标题、最后活跃时间）
2. 加载它，并把历史转成 UACP 事件流投给手机
3. 在其之上继续发起新的 turn

**验收口径（写进 G2）**：对每一家 agent，用它自己的 CLI 或 GUI 手工创建一个会话并说一句话，然后在 `kaleido-cli` 里必须能列出该会话、加载出刚才那句话、并接着提问得到回复。

### A-3 `live_attach` —— 实时接管另一个前端正在进行中的会话

**仅 OpenCode 在 v1 承诺。Codex 与 Claude Code 声明为不支持。**

指：桌面端正在跑一个 turn，手机同时连上去看到同一个 turn 的流式输出。

- **OpenCode**：`opencode serve` 是常驻服务，TUI 与 hostd 都是它的客户端，SSE 广播天然到达双方。hostd 应优先 attach 到已运行的实例（mDNS 或显式 URL），找不到才自己 spawn
- **Codex / Claude Code**：app-server 与 ACP 桥都是「每个客户端一个进程」的模型。另一个进程内存中正在进行的 turn 不经过我们，无法观测。**这是上游架构决定的，不是实现偷懒。** 会话结束落盘后可通过 A-2 恢复

### D-4 UI 与 trait 的约束

- `AdapterCaps` 中新增三个布尔位：`create`（恒为 true）、`resume`、`live_attach`
- UI **禁止**按 agent 名称判断是否显示「接管正在进行的会话」入口，必须读 `capabilities().live_attach`（`CLAUDE.md` §3.2 已有此纪律，此处重申）
- `REQUIREMENTS.md` §4.5 的 `list_sessions()` 注释「Tier A only」**作废** —— Claude Code 经 ACP `session/list` 同样支持。改为由 `caps.resume` 控制

---

## OBJ-1 的新表述

> **OBJ-1**：电脑上跑着 agent 就能远程控制
> **可验证的含义**：无论 agent 由 CLI 还是 GUI 启动，hostd 都能（A-1）新建会话、（A-2）列出并加载该 agent 会话存储中的历史会话并在其上继续对话。（A-3）实时接管另一个前端进行中的会话仅 OpenCode 支持，其余由 `capabilities()` 声明为不支持，UI 据此隐藏入口。

---

## 后果与风险

- **新增风险 R-9：会话存储的并发写入。** hostd 与用户的 CLI/GUI 可能同时操作同一个会话存储。Claude Code 的 `.jsonl` 与 Codex 的 thread 目录都没有公开的加锁约定。**缓解**：v1 内 hostd 加载会话后即持有该会话，UI 上明示「此会话已被手机接管」；G2 必须包含一条「hostd 与 CLI 同时打开同一会话」的冲突观测，把实际行为记录进 `docs/gates/G2-result.md`，不许假设它安全
- **新增风险 R-10：`loadSession` 是能力位，不是保证。** `claude-agent-acp` 当前支持，但这是适配器行为，可能随版本变化。hostd 必须在握手时检查 `loadSession` 与 `sessionCapabilities`，为 false 时优雅降级为「只能新建」并在 UI 明示原因 —— 不许崩溃、不许静默隐藏会话列表
- `session/load` 会重放全部历史，长会话可能一次性涌入大量事件。PROTOCOL 需要定义加载时的分页或背压策略（G1 处理）

## 被否决的方案

| 方案 | 否决理由 |
|---|---|
| 直接解析 `~/.claude/projects/*.jsonl` 自己重建历史 | 绕过官方协议，等同于「结构靠猜」，上游格式变更即失效。违反 OBJ-2 的精神 |
| 为 Codex / Claude Code 实现「轮询磁盘 + 伪造实时接管」 | 会产生延迟不可控、事件顺序不可靠的假象能力，比明确声明不支持更糟 |
| 把 A-3 从 v1 整体砍掉（包括 OpenCode） | OpenCode 天然支持，砍掉是浪费；且它是验证 UACP 能表达实时接管的唯一样本 |

## 影响的门禁

- **G1**：`AdapterCaps` 必须包含 `create` / `resume` / `live_attach`；PROTOCOL 需定义 `session/load` 的重放与背压语义
- **G2**：验收口径按 A-2 加严 —— 三家各做一次「外部 CLI/GUI 创建 → hostd 列出并加载 → 继续对话」；另加一条 R-9 冲突观测
