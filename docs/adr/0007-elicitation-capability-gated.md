# ADR-0007: Elicitation 降级为能力位控制（修正 ADR-0004 P-5）

- 状态：**已被 ADR-0010 取代**（2026-07-30）
- 保留价值：ACP v1 不含 elicitation 的核实证据；固定 12 事件与对应 proto 决策不再生效
- 起草：项目主管
- 触发：T-004 交付报告 + 主管对钉定 schema 的独立核实
- **取代**：[ADR-0004](0004-acp-version-pinning.md) P-5
- 影响：`docs/REQUIREMENTS.md` §4.5、§7.1

---

## 背景：一个由主管引入的错误

[ADR-0004](0004-acp-version-pinning.md) P-5 把 `Elicitation` 加入 v1 范围，理由是
「Codex 与 ACP 双方都有结构化表单输入能力」，并引用了 ACP 的 `elicitation/create` / `elicitation/complete`。

**这个依据是错的。** 那两个方法来自 ACP 文档站上的 **v2 Draft** 内容，
而本项目按 ADR-0004 P-2 钉定的是 **ACP v1**。

T-004 报告称「钉定 ACP v1.18 schema 根本没有 elicitation 定义」。主管对仓库内的
`schemas/acp/schema.json`（commit `48b2abf`，schema artifact 1.18.0）做了独立核实：

```
grep -i "elicit" schemas/acp/schema.json   →  无任何命中
```

并确认 ACP v1 的 `SessionUpdate` 实际只有 11 个变体：

```
user_message_chunk / agent_message_chunk / agent_thought_chunk /
tool_call / tool_call_update / plan / available_commands_update /
current_mode_update / config_option_update / session_info_update / usage_update
```

**结论：Tier B（Claude Code 经 ACP v1）在 v1 内无法表达 Elicitation。**
这不是适配器缺陷，是我们钉定的协议版本里就没有这个概念。

---

## 三家的实际情况

| Agent | Elicitation 能力 | 依据 |
|---|---|---|
| **Codex** | ✅ 有 `mcpServer/elicitation/request`（`mode: "form"` + `requestedSchema`） | `schemas/codex/` |
| **Claude Code（ACP v1）** | ❌ 协议层不存在 | `schemas/acp/schema.json` 实测无命中 |
| **OpenCode** | ⚠️ 疑似有等价物：会话的 `permission` 数组含 `question` 类型 | T-004 录制的 `opencode/08-session-load.jsonl:2` 可见 `{"permission":"question","pattern":"*","action":"deny"}` |

OpenCode 的 `question` 是否等价于结构化表单输入，**尚未有一手报文证据**，留待补录确认。

---

## 决策

### D-1 `Elicitation` 保留在 `kaleido-proto` 中，但降级为**能力位控制**

- `SessionEvent` 仍保留 `Elicitation` 变体 —— UACP 是我们自己的协议，不受 ACP v1 限制，
  且 Codex 确实会发这种请求，砍掉它 Codex 路径就会卡死
- **但它不再是「三家都必须支持」的 v1 硬性要求**
- `AdapterCaps` 增加 `elicitation: bool`。UI 依此决定是否渲染表单界面

### D-2 不支持的 adapter 必须优雅降级，不许静默丢弃

Claude Code adapter 在 ACP v1 下不会收到 elicitation 请求，因此没有降级路径需要写。
但**任何 adapter 若在运行时收到自己声明不支持的请求类型，必须产生一条 `Error` 事件**，
让用户看到「agent 要求输入但本通道不支持」，而不是静默卡死。

### D-3 `SessionEvent` 变体数维持 12 个

`REQUIREMENTS.md §4.5` 的 12 个变体不变。变的是**覆盖要求**：

> 12 个变体中，`Elicitation` 由 `caps.elicitation` 控制，不要求三家全覆盖。
> 其余 11 个仍要求三家全覆盖。

### D-4 契约测试的覆盖要求相应调整

`Elicitation` 的契约测试只对声明支持的 adapter 强制（当前仅 Codex）。
**不许因为「录不到」就把它从测试里删掉** —— 声明支持却拿不出 fixture 的，按缺证据处理。

### D-5 v2 迁移时重新评估

ACP v2 转 stable 后，若其 `elicitation/*` 稳定，另开 ADR 评估把 Claude Code 提升为支持。

---

## 主管的自我复盘（写进记录，避免重犯）

这个错误的根源是：**用文档站的说明当作协议真源，而不是用钉定版本的 schema 文件。**

文档站会同时展示多个版本的内容，且 v1/v2 的页面路径可能互相重定向。
`schemas/` 下的快照才是唯一凭证 —— 这正是 [ADR-0005](0005-schema-normalization-layer.md) D-2
「快照永不修改，它是唯一凭证」那条纪律存在的理由，只是我自己在签 ADR-0004 时没先去查它。

**今后规则**：任何涉及上游协议字段/方法名的 ADR，必须在 `schemas/` 快照中验证后才能签发，
并在 ADR 中写明验证命令。

---

## 影响的门禁

- **G1**：`AdapterCaps` 需含 `elicitation`；`PROTOCOL.md` 需定义不支持时的 `Error` 语义
- **G2**：Elicitation 的完整 turn 验收只对 Codex 强制
