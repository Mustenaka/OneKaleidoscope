# OneKaleidoscope 当前状态

> 生效日期：2026-07-30
> 状态：**文档重新定基线；产品代码暂停；没有可执行任务卡**

## 1. 当前结论

项目不是“完全不能做”，但旧路线把问题错误地定义成了“从任意已经运行的进程里提取完整数据”，
因此不断陷入 schema、fixture、进程发现和事件覆盖率的拉锯。

现在采用的路线是：

> **OneKaleidoscope 在 PC 上提供 Session Broker。它通过各家公开的结构化协议拥有或连接会话，
> 将会话状态、历史、进行中任务、审批、队列和跨 Agent 工作流投影到手机。**

终端 PTY、ANSI/TUI 抓屏、窗口文字识别、轮询 transcript 冒充实时协议，均不属于实现路径。

## 2. 不可降级的最终产品

最终产品必须同时满足：

1. 支持 Claude Code、Codex、OpenCode。
2. 支持每家的 CLI 与原生 GUI 表面。
3. 手机按 provider → 项目 → 会话查看历史和正在进行的会话。
4. 手机实时看到文本、推理、工具、计划、任务、diff、审批、问题和运行状态。
5. 手机可发送新消息、排队、steer、批准/拒绝、回答问题、取消与重试。
6. 支持 Claude Code 规划 → Codex 执行 → Claude Code 审核等跨 Agent 工作流。
7. 不通过终端转发获取 Agent 数据。
8. 编辑器/代码预览保留在产品范围，但不是当前纵切重点。

“公开接口暂时做不到”只会形成阻塞记录，不会改写上述目标，也不能算门禁通过。

## 3. 已确认与尚未确认

| Provider | Broker 管理的结构化会话 | 历史 | 外部原生 CLI 实时附着 | 外部原生 GUI 实时附着 |
|---|---|---|---|---|
| Codex | app-server 可支撑 | `thread/list/read/resume` 可支撑 | 需对共享实例做端到端证明 | **未发现稳定公开的 Desktop 发现/绑定合同** |
| Claude Code | Agent SDK 可支撑流式会话、审批和恢复 | SDK 可列出/读取/恢复 | 独立 CLI 进程没有公开第三方实时附着合同 | 官方 Remote Control 只面向 Anthropic 自有客户端；第三方接口不可依赖 |
| OpenCode | server REST + SSE 可支撑 | session/message API 可支撑 | `attach` + SSE 路径明确，仍需实录验收 | 需证明具体 GUI 版本连接同一 server |

这些结论按“具体 runtime 实例的能力”验证，不能按 provider 名称硬编码，也不能把“能读历史”
写成“能实时接管”。

## 4. 当前仓库处置

- `tests/fixtures/`、`schemas/`、`spikes/`：保留为一手证据和研究资产，不再作为开始产品代码的前置门禁。
- T-001～T-013：冻结为历史任务。
- T-014：在实现前撤销；它绕过了尚未定稿的规范化状态合同。
- `docs/tasks/M1-queue.md` 与根目录旧 `KICKOFF.md`：删除，避免继续下发失效队列。
- 旧 ADR：保留决策历史；被新 ADR 取代的内容不得作为当前合同。

## 5. 文档优先级

发生冲突时按以下顺序解释：

1. [REQUIREMENTS.md](REQUIREMENTS.md)
2. 已接受且未被取代的 ADR
3. [ARCHITECTURE.md](ARCHITECTURE.md)
4. [MILESTONES.md](MILESTONES.md)
5. 新任务卡
6. 冻结任务、旧队列、fixture 说明与 spike 文档

历史文件可以证明事实，不能重新定义范围。

## 6. 下一步

项目主管下一步必须先产出：

1. `docs/PROTOCOL.md`：规范化状态、命令、投影、cursor、能力和错误语义。
2. `crates/kaleido-proto`：与协议逐字一致的最小合同。
3. T-100 起的新任务卡：按端到端纵切拆分，不再恢复 T-001～T-014。

在上述三项通过合同评审前，不开始 adapter、reducer、hostd 或移动端产品代码。
