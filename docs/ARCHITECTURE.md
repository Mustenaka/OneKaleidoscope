# ARCHITECTURE — OneKaleidoscope 架构

> 状态：**v0.1 草案**（2026-07-29）。由项目主管产出。
> 与 `REQUIREMENTS.md` 冲突时以需求文档为准；本文件的变更走 ADR。
>
> **待 G0 定稿的部分**：§9 relay 的定位（可选组件 vs v1 必做）。其余部分不依赖 G0。

---

## 1. 一句话架构

> **三条 adapter 把各家私有协议归一化成 UACP 事件 → 事件先落 append-only 日志再出网 →
> iroh 加密通道投送到手机 → 手机按 cursor 拉齐。**

所有设计决定都服务于这条主线上的两个不变量：

| 不变量 | 出处 | 违反的后果 |
|---|---|---|
| **INV-1 事件先落盘，后发送**（write-ahead） | F-8 / G3「逐字节一致」 | 断线瞬间的事件可能已发出但未落盘，重放时凭空消失 |
| **INV-2 UACP 的类型不依赖任何上游协议的字符串** | ADR-0004 P-3 | 上游改名会穿透 proto 波及三端 UI |

---

## 2. 依赖方向（硬性，CI 应当强制）

```
                    ┌──────────────┐
                    │ kaleido-proto│  ← 合同。不依赖任何本项目 crate
                    └──────┬───────┘
             ┌─────────────┼──────────────┬────────────────┐
             ▼             ▼              ▼                ▼
      kaleido-transport  kaleido-adapter  kaleido-adapter-*  (mobile bindings)
             │           (trait 定义)      (codex/acp/opencode)
             └─────────────┬──────────────┘
                           ▼
                    kaleido-core        ← 会话状态机、事件日志、重放
                    ┌──────┴───────┐
                    ▼              ▼
              kaleido-hostd   kaleido-cli     (+ UniFFI → iOS / Android)
```

**规则**

1. `kaleido-proto` **不依赖**本项目任何其他 crate
2. `adapter-*` 之间**零依赖**。想复用就上提到 `kaleido-adapter`
3. `adapter-*` 只依赖 `kaleido-proto` + `kaleido-adapter` + 各自的上游 SDK
4. UI 层（hostd 托盘、cli、iOS、Android）**禁止**直接依赖任何 `adapter-*`
5. 任何 UI 分支必须读 `capabilities()`，**禁止按 adapter 名称硬编码**（`CLAUDE.md §3.2`）

> **执行方式**：`cargo xtask check-deps` 读 cargo metadata 对照本节的允许矩阵，
> 违反即 CI 失败。这条在 M3 第一张卡里落地。

---

## 3. crate 清单

| crate | 职责 | 不该出现在这里的东西 |
|---|---|---|
| `kaleido-proto` | UACP 全部类型；`serde` + `schemars`；版本/能力协商结构 | 任何 IO、任何 tokio、任何上游 SDK 类型 |
| `kaleido-adapter` | `AgentAdapter` trait、`AdapterCaps`、`AgentDiscovery` trait、进程监管的公共部分 | 具体 agent 的知识 |
| `kaleido-adapter-codex` | app-server JSON-RPC ↔ UACP | 其他 adapter 的类型 |
| `kaleido-adapter-acp` | ACP v1 ↔ UACP（服务 Claude Code 及其余 ACP agent） | 同上 |
| `kaleido-adapter-opencode` | REST + SSE ↔ UACP | 同上 |
| `kaleido-transport` | iroh endpoint、配对、L0~L2 分层与降级、连接状态上报 | 会话语义、事件语义 |
| `kaleido-core` | **事件日志（核心）**、会话状态机、重放、fs/git/diff 服务、UniFFI 导出面 | 平台专属路径逻辑（进 `platform/`） |
| `kaleido-hostd` | 进程生命周期、托盘/headless、配置、推送触发 | 协议实现 |
| `kaleido-cli` | G2 的验证客户端（含交互式审批 TUI） | 任何 hostd 才有的逻辑 |
| `kaleido-relay` | 配对验证、rendezvous、推送转发 | **任何明文业务内容** |

---

## 4. hostd 内部：事件流水线

这是整个项目最关键的一段，`REQUIREMENTS.md` 只在 §5 一句话带过，但它决定 F-8 能否成立。

```
 agent 进程
     │  各家私有报文
     ▼
┌─────────────────┐
│  adapter-*      │  归一化 → SessionEvent（尚无 seq）
└────────┬────────┘
         ▼
┌─────────────────────────────────────────┐
│  EventLog（kaleido-core）                │
│  1. 分配会话内单调递增 seq: u64          │
│  2. **fsync 落盘**（append-only）        │  ← INV-1 的执行点
│  3. 才向订阅者广播                       │
└────────┬────────────────────────────────┘
         ├──────────────► 本地订阅者（hostd 托盘 UI、CLI）
         └──────────────► transport → 手机
```

### 4.1 为什么日志必须在 adapter 与 transport 之间

- 放在 transport 之后：断线时正在途中的事件不会落盘 → 违反 INV-1
- 放在 adapter 之内：三家各自实现一遍定序逻辑 → 必然出现三种不同的 bug
- **只有放在中间，「不丢」才是结构性保证，而不是靠小心**

### 4.2 重放语义（`event.replay`）

| 保证 | 实现 |
|---|---|
| **不丢** | 日志 append-only 且写盘先于发送；任何被发出去过的事件必然在日志里 |
| **不重** | 客户端只接受 `seq > last_cursor`，其余丢弃（幂等） |
| **有序** | 服务端按 seq 严格升序重发 |

客户端重连发 `event.replay { session_id, since: last_cursor }`，服务端从日志取 `seq > since` 顺序重发。

**G3 的验收因此可机械化**：跑两遍同一会话（一遍不断线、一遍中途飞行模式 30s），
两份事件序列按 seq 排序后逐字节 diff 必须为空。

### 4.3 日志的落盘格式

- 每会话一个 append-only 文件：`<data_dir>/events/<session_id>.log`
- 一行一个事件，行首为 seq，便于 `seek` 与截断修复
- **不写入**：文件内容、工具参数明文、密钥（`REQUIREMENTS.md §6.3`）。
  大载荷（diff、文件内容）另存内容寻址存储，日志里只放引用

---

## 5. hostd 的双重 fs 角色（容易被忽略，且是安全高危区）

hostd 在文件系统上同时扮演两个角色：

```
   agent ──── fs/read_text_file ────►  hostd  ◄──── fs.* ──── 手机
           （ACP client 方法，我们是提供方）      （UACP，我们是提供方）
```

- **对 agent**：ACP 要求客户端实现 `fs/read_text_file`、`fs/write_text_file`、`terminal/*`。
  hostd 必须实现它们，否则 agent 无法完成工具调用（T-004 的 ACP 全列为空，
  最可疑的原因就是录制器把这些能力声明成了 `false`）
- **对手机**：`REQUIREMENTS.md §2` 端 A 规定「文件树、文件读取、diff 由 hostd 直接读盘，
  **不经由 agent 的 fs 能力**」

**两条路径共用一个受限的文件访问层**，统一执行：

1. 路径必须落在已授权的项目根内（防目录穿越）
2. 尊重 `.gitignore`（F-4）
3. **日志里只记路径的哈希与字节数，不记内容、不记完整路径**

---

## 6. Agent 发现与进程监管

按 [ADR-0006](adr/0006-agent-discovery.md)，`AgentDiscovery` 是一等公民，返回**结构化结果**而非布尔值：

```
显式配置 → 继承 PATH → 平台持久化环境变量 → 已知安装位置 → hostd 自备
```

**hostd 以托盘 / LaunchAgent / systemd user unit 启动，不执行用户的 shell profile**（R-12）。
因此发现失败时必须能报告「五处分别看到什么」，UI 呈现发现来源而非可用/不可用。

### 6.1 发现不是找到「一个」二进制，而是在多个候选中做选择（R-13）

T-006 实测：同一台机器上 `codex` 至少有三个实例 —— Store/GUI 版、npm CLI 版、npx 临时版，
**各自持有独立的登录态**。`where.exe codex` 命中的是 Store GUI 版，在录制进程里返回
`Not logged in`；而用户终端里的 CLI 版是 `Logged in using ChatGPT`。

**推论：`AgentDiscovery` 返回的是候选列表，不是单个路径。** 每个候选必须携带：

| 字段 | 用途 |
|---|---|
| 路径与来源层级 | 用户能看懂「从哪找到的」 |
| 版本 | 与 `schemas/` 快照对照 |
| **登录态** | 决定它能不能真的干活 |
| 选中理由 | 排障时最关键的一条 |

选择策略：**已认证 > 版本匹配快照 > 来源层级靠前**。
UI 必须显示「选中了哪一个、为什么」，并允许用户显式指定（发现优先级①）。

> 这条对 OBJ-1 是硬约束：用户在终端里看到「已登录」，hostd 却报「未登录」，
> 而且给不出原因 —— 这是最难自查的一类故障。

进程监管的平台差异（`REQUIREMENTS.md §2` 端 A）：

| 关注点 | 收敛位置 |
|---|---|
| `.cmd` / `PATHEXT` 解析、`CREATE_NO_WINDOW`、杀进程树 | `kaleido-adapter/src/platform/windows.rs` |
| 配置/数据目录 | `directories` crate，**禁止手写字面量** |
| 文件监听 | `notify` crate |

> 进程清理只针对**本次 spawn 的进程树**（按 PID 家族），
> 绝不按可执行文件名匹配全系统进程 —— 否则用户开着 GUI 就无法工作。

---

## 7. UniFFI 边界

**边界画在 `kaleido-core` 的外沿。** 手机侧只拿到会话语义，不碰协议细节。

### 7.1 跨 FFI 的类型

| 跨 | 不跨 |
|---|---|
| `SessionEvent`（12 个变体，enum with payload） | `AgentAdapter` trait 对象 |
| `SessionSummary` / `AdapterCaps` / `ConnectionLayer` | 任何 `tokio` 类型 |
| `PermissionRequest` / `Decision` | 任何 `BoxStream` |
| `Cursor`（u64 newtype） | 上游 SDK 的任何类型 |

### 7.2 async 流的桥接（R-5 的正面处理）

UniFFI 对 `Stream` 表达力有限。**不把 `BoxStream` 直接跨 FFI**，改用回调 trait：

```
Rust 侧：core 持有事件流 → 逐条调用 EventSink::on_event(SessionEvent)
Swift  ：实现 EventSink，内部转 AsyncStream
Kotlin ：实现 EventSink，内部转 Flow
```

**硬性要求（ADR-0001 D-6 / ADR-0002 D-2）**：G1 必须用真实类型
**同时生成 Swift 与 Kotlin 绑定并双双编译通过**。只验证 Kotlin 是不允许的 ——
Android 先行指的是 UI 与真机验收先行，不是核心类型只对 Android 负责。

---

## 8. 传输分层与降级

```
L0 局域网 mDNS + iroh 直连   （iroh-mdns-address-lookup，不需要 relay）
L1 iroh NAT 打洞             （仅需 rendezvous）
L2 iroh relay 加密中转       （零知识）
L3 用户自备隧道              （Tailscale / Cloudflare，用户自理）
```

- 客户端自动按 L0 → L1 → L2 尝试，**UI 明示当前层级**（`REQUIREMENTS.md §6.1`）
- 层级判定用 iroh 1.0 的 `Connection::paths()` + `Path::is_ip()` / `is_relay()` / `is_selected()`
  （`conn_type()` 在 1.0 已删除，见 ADR-0001 C-1）

---

## 9. relay 的定位 —— **待 G0 定稿**

| G0 结果 | relay 定位 | 影响 |
|---|---|---|
| 直连成功率 **≥ 60%** | 可选组件。局域网与多数外网场景不部署也能用 | `kaleido-relay` 排在 M8 |
| 直连成功率 **< 60%** | **v1 必做**。默认路径就要过 relay | `kaleido-relay` 提前到 M4，零知识约束成为 G3 的验收项 |

**本节在 G0 数字拿到前不填。** 这也是 `MILESTONES.md` 把 G0 排在最前的唯一理由。

---

## 10. 反模式清单（审核时逐条查）

| # | 反模式 | 依据 |
|---|---|---|
| A-1 | ANSI 转义解析 / 屏幕抓取获取 agent 输出 | OBJ-2，立项前提 |
| A-2 | UI 按 agent 名称分支，而非 `capabilities()` | `CLAUDE.md §3.2` |
| A-3 | 事件先发后写盘 | 违反 INV-1，F-8 失效 |
| A-4 | UACP 复用上游协议的判别字符串 | 违反 INV-2，ADR-0004 P-3 |
| A-5 | 平台专属逻辑散落在跨平台模块 | `AGENTS.md §3.5` |
| A-6 | 手写上游类型 | `AGENTS.md §3.2`（规范化层例外见 ADR-0005） |
| A-7 | 日志/推送载荷含文件内容、工具参数、密钥、完整用户路径 | `REQUIREMENTS.md §6.3` |
| A-8 | 因 PATH 上没有 CLI 就判定 agent 不可用 | ADR-0006 D-3 |
| A-9 | 按可执行文件名匹配全系统进程做清理 | 本文 §6 |
| A-10 | adapter 之间互相依赖 | 本文 §2 |

---

## 11. 尚未定稿的部分

| 项 | 阻塞于 | 预计 |
|---|---|---|
| relay 定位（§9） | **G0** | 20 轮实测 |
| `SessionEvent` 各变体的确切字段 | **T-006 的 fixture** | 补录完成 |
| 权限审批的统一形状 | T-006 的 P0 场景（R-8） | 已有 schema 骨架，缺字段可选性证据 |
| 大载荷（diff/文件）的内容寻址存储细节 | `PROTOCOL.md` | M2 后半 |
| 推送载荷的确切结构 | `PROTOCOL.md` + APNs/FCM 双端约束 | M2 后半 |
