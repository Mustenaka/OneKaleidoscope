# 同类项目与官方协议经验

> 核对日期：2026-07-30
> 用途：帮助项目主管选实现模式，不把竞品宣传语直接当成协议事实。

## 1. 官方能力边界

### Codex app-server

[官方 app-server 文档](https://github.com/openai/codex/blob/main/codex-rs/app-server/README.md)
说明它是丰富客户端使用的双向 JSON-RPC 接口，支持 stdio，并提供实验性的 WebSocket 和 Unix socket。
它适合由 OneKaleidoscope 启动/连接并作为结构化会话后端。

可借鉴：

- Thread → Turn → Item 的对象层级；
- 双向 request/notification；
- approval、turn steer/interrupt、plan/diff/status 等可投影能力；
- 多订阅和断线恢复应围绕稳定对象 ID，而不是终端文本。

不可推断：

- 一个外部 app-server 客户端必然能发现并绑定 Codex Desktop 当前私有实例；
- 第一方 Remote Control 的内部注册/路由协议对第三方开放。

### Claude Agent SDK

[Sessions](https://code.claude.com/docs/en/agent-sdk/sessions)、
[Agent loop](https://code.claude.com/docs/en/agent-sdk/agent-loop) 和
[Permissions](https://code.claude.com/docs/en/agent-sdk/permissions) 提供结构化流式消息、session
恢复/枚举、工具生命周期和交互式审批，适合 Broker 管理的 Claude 会话。

[Claude Remote Control](https://code.claude.com/docs/en/remote-control) 连接的是 claude.ai/code
与 Anthropic 自有手机 App，流量经 Anthropic 服务。它是交互参考，不是 OneKaleidoscope 可调用的第三方协议。

经验：

- Claude 主路径应以 Agent SDK 为基础；
- hooks 可补生命周期观测，但不能替代完整流式会话协议；
- 原生独立 CLI/GUI 的第三方实时附着必须单独证明，不能从“session 可恢复”推导。

### OpenCode server

[OpenCode Server](https://dev.opencode.ai/docs/server/) 提供 REST、`/global/event` SSE、session/message
查询和异步 prompt。它天然接近 shared runtime。

经验：

- 让 TUI/GUI/Broker 都成为同一个 server 的客户端；
- 用 SSE cursor/重连语义验证多客户端同步；
- 不要只生成 OpenAPI 客户端而忽略事件流和运行时所有权。

## 2. 开源同类项目

### Zane

[Zane](https://github.com/z-siddiqi/zane) 的公开架构是：

```
Phone/Web → Cloudflare relay → local Anchor → Codex app-server JSON-RPC
```

Anchor 启动 app-server，relay 负责认证、WebSocket 和推送。最重要的经验不是它解析了多少 Codex
事件，而是它**拥有本地会话入口**，手机控制的是这个入口。

可借鉴：

- 本地 daemon + 外部 relay + 手机的三层结构；
- outbound-only 连接、推送、审批与 diff；
- self-host 和 local mode。

不直接复制：

- 单 provider 模型；
- Cloudflare 特定基础设施；
- 未认证 local mode；
- 把 wrapper 管理会话等同于任意 Codex Desktop 会话。

### Happy

[Happy](https://github.com/slopus/happy) 要求用户运行 `happy claude` / `happy codex`，由 wrapper
创建并切换远程会话。它验证了“安装一次集成入口”是可接受且可工作的产品路径。

可借鉴：

- wrapper/launcher 明确拥有未来会话；
- 手机和本地键盘切换控制权；
- E2EE、推送、轻量移动事件协议。

不直接复制：

- restart/session switching 不等于同一活动 runtime 的多客户端订阅；
- 它没有解决任意未包装原生 GUI 进程的公开附着；
- OneKaleidoscope 不能用终端接管作为兜底。

### Happier

[Happier](https://github.com/happier-dev/happier) 覆盖多 provider、daemon、移动/桌面/Web、
自托管 relay、Inbox、Pending Queue、steering、项目/文件/Git。它与目标产品最接近。

最值得吸收的产品结构：

- Machine Daemon 是长期控制平面；
- 全局 Attention Inbox；
- Pending Queue 与运行中 steering 分开；
- provider 能力变化由服务端声明；
- 项目、会话、worktree、Agent/subagent 是不同对象；
- relay 自托管且业务内容端到端加密。

需要警惕：

- README 中“follow/take over existing sessions”是产品声明，不等于每家原生 GUI 都有同样公开协议；
- 部分持久会话使用 tmux-backed resume，这不符合本项目禁止终端转发的约束；
- 功能面极宽，照搬会让首个纵切再次失焦。

### codex-acp

[codex-acp](https://github.com/zed-industries/codex-acp) 证明可以把 Codex 接到 ACP 客户端生态，
但 ACP 是互操作层，不应把 Codex app-server 或 Claude Agent SDK 的丰富能力削成最低公分母。

## 3. 对 OneKaleidoscope 的直接结论

1. **会话所有权先于事件提取。** 成功项目都通过 daemon/wrapper/shared server 控制会话入口。
2. **relay 是产品组件。** 移动网络与后台限制决定了必须有可靠回退和 push。
3. **移动端围绕状态投影。** Inbox、queue、workflow、session status 比上游事件枚举更稳定。
4. **provider 用各自最强公开协议。** 统一发生在 canonical state，不发生在传输最低公分母。
5. **原生表面能力逐格取证。** wrapper 成功不能自动证明官方 GUI 实时附着成功。
6. **先做窄纵切。** 一个 provider + hostd + Android 的真实任务，比三家完整 schema 更能验证架构。
7. **不复制不合规兜底。** tmux、PTY、ANSI、窗口抓取和 transcript 轮询均不得进入产品路径。
