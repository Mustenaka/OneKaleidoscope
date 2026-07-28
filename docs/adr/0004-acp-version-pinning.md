# ADR-0004: ACP 接入的版本与包名钉定

- 状态：**提案（待负责人确认）**
- 起草：项目主管（2026-07-28）
- 影响：`docs/REQUIREMENTS.md` §4.2、§4.5、§9 R-2；`docs/PROTOCOL.md`

---

## 背景

`REQUIREMENTS.md` §4.2 指定 Claude Code 经 `npx @zed-industries/claude-code-acp` 接入，Rust 侧使用官方 `agent-client-protocol` crate。核对官方源后发现两件事：

### 事实 1：适配器包已废弃并改名

- `@zed-industries/claude-code-acp`（最新 0.16.2）**每个版本都带废弃标记**：「This package has been renamed to `@agentclientprotocol/claude-agent-acp`. Please migrate to continue receiving updates.」
- 新包：`@agentclientprotocol/claude-agent-acp`，最新 **0.63.0**，依赖 `@agentclientprotocol/sdk` 0.25.0 与 `@anthropic-ai/claude-agent-sdk` 0.3.169
- 仓库从 `zed-industries/claude-code-acp` 迁至 `agentclientprotocol/claude-agent-acp`

版本号从 0.16 跳到 0.63，说明改名后经历了大量迭代。**继续用旧包 = 用一个停止更新的适配器。**

### 事实 2：ACP 规范本身正处在 v1 → v2 过渡期

- 协议 **v1 为 stable，v2 标注为 Draft**
- Rust crate `agent-client-protocol` 已发布 **2.0.0（2026-07-23，五天前）**，此前 1.3.0（07-20）、1.2.0（07-07）
- **`session/update` 的变体判别字符串在两版之间改了**：v1 文档中的 `agent_message_chunk` / `tool_call_update`，在 v2 schema 中变为 `content_chunk` / `tool_call_result`

这不是 `REQUIREMENTS.md` §9 R-2 描述的风险（「适配器落后于 CLI」），而是**规范本身在动**。`REQUIREMENTS.md` §4.5 那份「SessionEvent 至少覆盖 11 个变体」的清单，其名称直接来源于 ACP v1；若 UACP 跟着 ACP 漂移，`kaleido-proto` 这份「合同」就失去了合同的意义。

### 事实 3：§4.5 的变体清单存在缺口

Codex 与 ACP 双方都有**结构化表单输入**能力：

- Codex：`mcpServer/elicitation/request`（`mode: "form"`，带 `requestedSchema`）
- ACP：`elicitation/create` / `elicitation/complete`

§4.5 列出的 11 个变体中没有对应项。MCP server 走 OAuth 或需要用户填参数时，手机端会收到一个无法渲染的请求，会话就此卡死。

---

## 提案

### P-1 包名与版本钉定

- Claude Code 适配器改用 **`@agentclientprotocol/claude-agent-acp`**，在配置中**钉死一个确切版本号**（不用 `latest`、不用 `^`）
- hostd 启动时探测 Node 与该包；缺失时给出确切的安装命令，不许静默失败
- 升级适配器版本必须走 ADR

### P-2 ACP 版本钉定在 v1，Rust crate 钉在 1.x

**理由**：v2 仍是 Draft；`claude-agent-acp` 当前依赖的是 `@agentclientprotocol/sdk` 0.x 线。在上游未稳定前跟进 v2 会让我们同时承担规范变更与适配器变更两份风险。

**待 v2 转 stable 后另开 ADR 评估迁移。**

### P-3 UACP 不复用 ACP 的判别字符串

`kaleido-proto` 的 `SessionEvent` 使用**我们自己的判别值**，与 ACP 的字符串解耦。adapter 层负责映射。

**理由**：这是让 ACP 版本变更只影响一个 adapter crate、不波及 proto 与三端 UI 的唯一办法。代价是 adapter 层多一张映射表 —— 这张表恰好是契约测试的靶子。

### P-4 schema 快照落进仓库

在 `schemas/` 下保存所用版本的：

- ACP JSON Schema（v1）
- `codex app-server generate-json-schema` 的输出
- OpenCode `/doc` 的 OpenAPI 3.1 spec

CI 每日重新拉取并 diff（对应 R-4 的缓解措施）。**diff 非空即告警**，让上游 breaking change 在编译失败之前就被发现。

### P-5 §4.5 的事件变体清单补充 `Elicitation`

`SessionEvent` 增加一个变体，承载「agent 要求结构化输入」：至少包含请求 id、提示文本、JSON Schema、以及取消语义。对应的客户端响应方法进入 `permission.*` 或独立的 `elicit.*` 方法族（G1 定夺）。

**否则 MCP OAuth 场景在手机端必然卡死。**

### P-6 R-2 重新表述

> **R-2**：ACP 规范自身处于 v1→v2 过渡，且 Claude Code 适配器迭代频繁
> **影响**：UACP 事件语义漂移；adapter 编译失败
> **缓解**：ACP 版本与适配器版本双钉定（P-1/P-2）；UACP 判别值与 ACP 解耦（P-3）；schema 快照 + CI 每日 diff（P-4）

---

## 需要负责人确认的点

1. 是否同意把 ACP 钉在 **v1 / crate 1.x**，暂不跟进 v2 Draft？
2. 是否同意 **P-5** 把 `Elicitation` 加进 v1 范围？它会让 §4.5 的变体从 11 个变成 12 个，手机端多一个表单渲染界面 —— 这是 v1 范围的实质扩大，需要你点头。

## 影响的门禁

- **G1**：`kaleido-proto` 的 `SessionEvent` 变体数量与判别值取决于 P-3 / P-5
- **G2**：Claude Code 的完整 turn 验收必须基于钉定版本的新包
