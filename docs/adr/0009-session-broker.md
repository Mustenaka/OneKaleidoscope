# ADR-0009: hostd 是本机 Session Broker

- 状态：**已接受**（2026-07-30）
- 决策人：项目负责人
- 起草：项目主管
- **取代** [ADR-0003](0003-agent-attach-semantics.md) 的能力模型（A-1/A-2/A-3 三位制作废，其一手证据保留）
- 影响：`REQUIREMENTS.md` §1.1 / §4 / §8 / 新增 §11、`ARCHITECTURE.md`、`MILESTONES.md`

---

## 1. 为什么要推翻原来的框架

原来的隐含框架是：**扫描本机任意 agent 进程，设法把它的数据「提取」出来。**

这个框架下，「实时接管一个已经独立运行、没有任何前置集成的 GUI 进程」在三家上都没有公开路径。
于是 [ADR-0003](0003-agent-attach-semantics.md) 把 OBJ-1 软化成了「Tier B 至少可新建」——
**那是拿需求去迁就实现，是本项目至今最严重的一次管理错误。**

正确框架是：

> **hostd 成为本机统一的 Session Broker。三家 agent 的 GUI 与 CLI 连接或注册到这个共享会话层，
> 手机只连 hostd。**

这不是降低需求，而是实现「实时、双向、不走终端转发」的**必要条件**。

---

## 2. 已验证的事实（`codex app-server --help`，0.146.0）

Broker 前提在 Codex 上有**原生支持**，不是我们硬凑的：

```
Commands:
  daemon   Manage the local app-server daemon
  proxy    Proxy stdio bytes to the running app-server control socket

  --listen <URL>   stdio:// (default) | unix:// | unix://PATH | ws://IP:PORT | off
  --ws-auth <MODE> capability-token | signed-bearer-token
  --ws-token-file / --ws-token-sha256 / --ws-shared-secret-file
  --ws-issuer / --ws-audience / --ws-max-clock-skew-seconds
  --code-mode-host <WS_URL>   Connect to a remote code-mode host instead of starting a local host
```

推论：

1. **共享 daemon 是一等公民**（`daemon` 子命令）
2. **多客户端接同一 daemon 是设计意图**：`proxy` 明确写「连接到*正在运行的* app-server
   control socket」；这也解释了快照里为何存在 `thread/unsubscribe`
3. **「从别处连进来」在设计范围内**：非 loopback WS 有完整的 capability-token / 签名 JWT 鉴权

配套地，`schemas/codex/` 0.146.0 快照已核实存在（主管逐条 grep 验证）：

| 方法 | 服务于用户需求的哪一项 |
|---|---|
| `turn/steer` | **引导进行中的 turn** |
| `turn/plan/updated` | 进行中的任务/计划 |
| `turn/diff/updated` | diff 实时更新 |
| `turn/interrupt` | 取消 |
| `thread/loaded/list` | **正在进行中的对话** |
| `thread/list` / `thread/read` / `thread/resume` | 项目内的历史对话 |
| `thread/status/changed` | 会话运行状态 |
| `thread/unsubscribe` | 多订阅者 |

**用户要求的绝大部分能力，Codex 原生就有。是我们此前的 12 事件模型把它们全漏了。**

> `subagent/*` 在 Codex 快照中**不存在**（0 命中）。任何依赖它的设计不成立。

---

## 3. 决策

### D-1 三层拓扑

```
手机 App ──iroh──► hostd (Session Broker) ──► Provider Runtime
                        │                        ├─ Codex: 共享 app-server daemon (unix:// / ws://)
                        │                        ├─ OpenCode: 常驻 server (HTTP + SSE)
                        │                        └─ Claude: Agent SDK / claude-agent-acp
                        └─ EventLog (append-only, write-ahead)
```

- **手机永不直连 provider runtime**，只连 hostd
- hostd 是唯一持有 provider 凭据与会话所有权的进程

### D-2 每个会话必须区分两个独立概念

| 概念 | 含义 |
|---|---|
| **`history_source`** | 历史从哪里读（磁盘 store / provider API） |
| **`live_runtime`** | 当前由哪个具体运行时执行，能否订阅与控制 |

ADR-0003 把这两件事压成了 `resume` / `live_attach` 两个布尔位，
导致「能列出 GUI 创建的历史」被反复误读成「接管了 GUI 正在跑的会话」。**这个压缩作废。**

### D-3 能力按「具体 runtime 实例」协商，不按 agent 名字

能力位细化为（取代 A-1/A-2/A-3）：

```
spawn_owned              hostd 自己创建会话
resume_persisted         恢复任意来源创建的历史会话
observe_external_live    订阅由别的前端启动、正在进行的 turn
control_external_live    对上述 turn 发 prompt / steer / interrupt
simultaneous_multi_client 原生表面与手机同时在线且互相可见
steer_in_flight          turn 进行中注入引导
approval                 权限审批
queue_visibility         用户输入队列状态可见
plan_visibility          agent 工作计划/任务状态可见
```

**这些位由 hostd 在连上具体 runtime 后探测得出，不许按 agent 名字硬编码**
（延续 ADR-0008 D-5：运行时能力优先于版本号与名称）。

### D-4 「一次性启用集成」是已接受的产品前提

负责人已明确接受。产品承诺表述为：

> 完成一次性集成启用后，由任意表面（CLI / GUI）创建的会话都进入 hostd 管理的共享运行时，
> 手机可实时查看与控制。启用前已存在的会话可查看历史并恢复，但不承诺实时接管。

**这是前提，不是缺口。** 文档中不得再把它写成「能力不足」。

### D-5 不依赖厂商的 remote-control 协议

负责人明确：无法要求 OpenAI / Anthropic 开放第三方 remote-control 接口。

因此 **`remoteControl/*` 与 Claude 官方 `/remote-control` 均不作为技术路线**。
它们只作为**竞品交互参考**（M5 的 UI 设计输入）。

传输由我们自己拥有：iroh P2P + 负责人自有的 Ubuntu 服务器作为 relay / rendezvous。
**OBJ-3（不经第三方 SaaS）因此完整保住** —— relay 是负责人自己的机器。

### D-6 上游阻塞登记制度（本 ADR 的机制保障）

**需求永不因实现困难而降级。** 上游确实没有公开路径的能力，进入
`REQUIREMENTS.md §11 上游阻塞登记`，记录：

- 缺什么能力、影响哪条需求
- 已验证到什么程度（附证据）
- 当前替代方案
- 复查触发条件（上游版本更新 / 官方文档变化）

**门禁不得因为登记了阻塞就算通过**，而是标注「该项受 UB-N 阻塞」，需求条目保持原样。

### D-7 跨 agent 编排是 Broker 的职责

负责人补充的核心使用场景：

> 在 PC 上编排好 plan 丢给工作机执行，人离开桌面，随时用手机查看执行情况并干预。
> 并且支持跨 agent 模型的工作模式（Claude Code 编排 plan → Codex 执行 → Claude Code 审核），
> 一个手机端处理完成。

这要求 hostd 在**会话之上**再有一层：同一项目内跨 provider 的任务交接。
Broker 架构是它的必要条件（没有共享会话层就无法交接）。

**v1 范围**：Broker 必须让「同一项目下不同 provider 的会话」在手机上归组可见并可切换。
**跨 provider 的自动化交接（编排引擎）不在 v1**，但 proto 与 Broker 的数据模型
不得排除它 —— 项目/会话/任务的层级必须留出位置。

---

## 4. 唯一剩下的关键未知

**Codex Desktop（GUI）是连接这个共享 daemon，还是自己起一个私有 app-server？**

- 连 daemon → `thread/loaded/list` 能看到 Desktop 正在用的 thread，
  `observe_external_live` / `control_external_live` 对 Codex **成立**，用户需求完整可达
- 私有 → Broker 只覆盖 CLI，Desktop 那半进 §11 登记

这条**必须实测**，不许从文档推断（主管已在 Elicitation 上犯过一次这个错，见 ADR-0007 自我复盘）。
验证方式见 `docs/tasks/T-013-broker-spike.md`。

---

## 5. 被否决的方案

| 方案 | 否决理由 |
|---|---|
| 继续「扫描进程 + 提取数据」 | 对未集成的 GUI 无公开路径；已导致需求被软化 |
| 走厂商 remote-control 协议 | 第三方协议未公开，且对端是厂商服务器，撞 OBJ-3。负责人无法推动上游 |
| 解析 TUI / PTY / ANSI 补缺口 | 违反 OBJ-2 立项前提，无商量 |
| 直接解析 transcript JSONL 冒充实时 | 历史导入可以，冒充实时协议不行 |
| 按更新时间 / rollout 文件 / 窗口标题猜「当前活动会话」 | 不可靠的启发式，一定会在真机上出错 |
| 为了让门禁通过而修改需求 | ADR-0003 已经犯过，本 ADR 就是为了纠正它 |

---

## 6. 影响的门禁

**G2 起，门禁不再以「录到多少事件变体」判定，改为 3 家 × CLI/GUI 的端到端场景**：

原生表面启动 turn → 手机数秒内看到 running → 看到文本/工具/计划/diff 增量 →
手机审批或 steer → 原生表面同步反映 → 断网重连后状态与队列一致。

某家某表面受 §11 阻塞时，该格标注阻塞编号，**不得改判为通过**。
