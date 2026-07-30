# ADR-0009: hostd 采用 Session Broker 架构

- 状态：**已接受，2026-07-30；本版取代同日早期草案**
- 决策人：项目负责人
- 取代：[ADR-0003](0003-agent-attach-semantics.md) 的三级布尔能力模型

## 背景

旧路线假定可以发现任意已运行的 Claude Code、Codex、OpenCode 进程，再从中“提取”实时数据。
公开协议并不普遍提供这种跨进程发现和附着能力。该假定造成了三个后果：

1. 把历史文件、进程发现和实时协议混在一起；
2. 为了追求完整事件矩阵，不断录制和修补 schema 工具；
3. 遇到原生 GUI 缺少公开入口时，反过来降低产品需求。

## 决策

### D-1 hostd 是控制平面

hostd 负责拥有或连接 provider runtime、规范化状态、持久化、工作流、移动端投影和远程命令。
手机只连接 hostd，不直接实现三家协议。

### D-2 会话分三种所有权模式

- `broker_managed`：hostd 通过公开协议创建并拥有会话；
- `shared_runtime`：hostd 与原生表面连接同一公开 server；
- `external_native`：原生表面独立运行，仅在公开协议允许时附着。

“一次性启用 OneKaleidoscope 集成”可以安装 launcher、daemon、插件或共享 server 配置，
但不能假设未公开的厂商行为。

### D-3 历史与实时分开

每个会话独立记录 `history_source` 和 `live_runtime`。历史可列出、可读、可恢复，均不能证明
另一个进程当前的 turn 可实时观察或控制。

### D-4 能力属于 runtime

能力在连接时协商并携带证据，不能按 provider 名称或版本写死。UI 必须明确区分：

- 支持；
- 不支持；
- 当前连接不可用；
- 尚未验证；
- 受上游公开接口阻塞。

### D-5 不依赖厂商 Remote Control 私有接口

Claude 官方 Remote Control 与 Codex 第一方 Remote Control 可以作为交互参考，但不是第三方集成合同。
本项目不等待 OpenAI/Anthropic 提供合作接口，也不逆向其私有协议。

### D-6 原生 GUI/CLI 仍是最终验收

Session Broker 不是把“原生 GUI/CLI”改写为“我们自己的 wrapper UI”。三家 × 两种表面仍保留六个验收格。
某格缺少公开实时路径时，产品保持未完成并登记阻塞，不用终端转发、磁盘轮询或隐藏入口代替。

## Provider 当前路径

| Provider | Broker 管理 | 外部原生表面 |
|---|---|---|
| Codex | app-server JSON-RPC | CLI/GUI 是否可连接同一公开实例必须分别证明；Desktop 当前没有稳定公开绑定合同 |
| Claude Code | Agent SDK | 独立 CLI/GUI 的第三方实时附着当前无公开合同；历史能力单独验证 |
| OpenCode | server REST + SSE | 优先共享 server；TUI 与 GUI 分别验收 |

ACP 是兼容层，不再被当作三家必须共同服从的最低公分母。

## 后果

- 产品开发从一个 Broker 管理纵切开始，不从全量进程发现开始；
- provider fixture 变成按需契约证据，不再是产品开工门禁；
- 原生表面缺口与 Broker 管理会话实现分开排期；
- 规范化合同必须先定义状态和投影，再定义 provider 映射；
- [REQUIREMENTS.md](../REQUIREMENTS.md) 的最终范围不因阻塞改变。

## 否决方案

| 方案 | 原因 |
|---|---|
| 扫描任意进程并猜当前会话 | 没有稳定身份和订阅合同 |
| transcript 轮询冒充实时 | 顺序、等待状态、审批与工具生命周期不可靠 |
| PTY/tmux/ANSI 转发 | 违反立项前提 |
| 依赖厂商私有 Remote Control | 无公开第三方合同，且传输不可控 |
| 把 wrapper 会话称为原生 GUI 已支持 | 改写验收语义 |
