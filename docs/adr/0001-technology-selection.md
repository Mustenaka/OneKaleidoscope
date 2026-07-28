# ADR-0001: 技术选型固化

- 状态：**已接受**（2026-07-28）
- 决策人：项目负责人
- 起草：项目主管
- 影响：`docs/REQUIREMENTS.md` §3、§4、§6

---

## 背景

`REQUIREMENTS.md` §3 以表格形式锁定了技术选型，但未记录**被否决的方案与否决理由**。缺少这部分记录会导致后续实现方（或未来的自己）反复重开已经关闭的讨论，也无法判断某个选型在什么条件下应该被重新审视。

本 ADR 把 §3 的选型固化，补上否决理由与「重新审视的触发条件」，并修正在核对官方文档时发现的三处事实性偏差。

---

## 决策

### D-1 PC 端与核心逻辑用 Rust，单一 workspace 覆盖三平台

**否决方案**

| 方案 | 否决理由 |
|---|---|
| Go | iroh 只有 Rust 实现；ACP 官方 SDK 是 Rust；跨端复用需要另找 FFI 方案 |
| TypeScript / Node | 常驻内存占用高；分发需要带运行时；与 iroh / ACP 生态不匹配 |
| 三端各写一套 | 直接违反 OBJ-4，且必然出现类型漂移（R-7） |

### D-2 跨端复用走 UniFFI，不走 REST / 本地 HTTP

**否决方案**

| 方案 | 否决理由 |
|---|---|
| 手机端起本地 HTTP server，Swift/Kotlin 走 HTTP 调 Rust | 多一层序列化；协议逻辑会不可避免地渗回 Swift/Kotlin（违反 AGENTS.md §3.3/§3.4） |
| 手机端用 C ABI 手写绑定 | 手写绑定就是手写类型，等于放弃「单一真源」 |
| Kotlin Multiplatform / Swift on Android | 无法复用 iroh |

**已知风险**：UniFFI 对泛型与 async 流的表达能力有限（R-5）。缓解见 D-6。

### D-3 传输层用 iroh（QUIC），NodeId 即身份

**否决方案**

| 方案 | 否决理由 |
|---|---|
| 自建 WebSocket + TLS + 自签 CA | 需要 CA 与证书 pinning，配对复杂度陡增；无法直连，必须过服务器（违反 OBJ-3 的成本假设） |
| WebRTC DataChannel | 信令与 ICE 栈重；Rust 生态不成熟；移动端集成成本高于 QUIC |
| 只做 Tailscale / Cloudflare Tunnel | 强制用户依赖第三方账号；违反「零配置」目标。保留为 L3 逃生通道 |
| libp2p | 依赖体积与概念负担远超需求；打洞能力并不优于 iroh |

**重新审视的触发条件**：门禁 G0 实测直连成功率 < 60%（则 L2 relay 升为必做，但选型本身不变）；或 iroh 1.x 出现无法接受的 breaking change。

### D-4 走结构化协议，不走 PTY 转发

这是立项前提（OBJ-2），此处记录否决理由以备将来。

**否决方案：PTY / 终端转发**

- Agent 的 TUI 输出是**给人看的**，没有稳定性承诺，上游任何一次 UI 改版都会打断解析
- 工具调用参数、diff、权限请求在 TUI 里是渲染结果，**结构信息已经丢失**，无法在手机端重建成可交互卡片
- 权限审批需要「哪个工具、什么参数、批准还是拒绝」的结构，屏幕抓取拿不到
- 事件重放（F-8）需要离散、可定序的事件，字符流无法满足

**保留为 v2 逃生通道**：当某个 agent 完全没有结构化协议时，PTY 可作为最后手段，但绝不能是 v1 主路径。

### D-5 Claude Code 走 ACP 桥，不直连 `stream-json`

**否决方案：`claude --output-format stream-json`**

- 该格式未公开承诺稳定
- 直连意味着我们自己扛住上游每一次变更；走 ACP 桥则由上游适配器承担
- 走 ACP 意味着「新增一个 ACP agent 不需要改 proto」（OBJ-5）自动成立，覆盖面从 1 家扩到 40+ 家

**代价**：功能上限被 ACP 交集限制（Tier B）；多一个 Node 进程依赖。保留为 plan B（R-2）。

### D-6 R-5 的缓解前置到 G1

UniFFI 表达能力受限的风险不能等到 G4（iOS）才暴露。**G1 的 Definition of Done 中必须包含：用 `kaleido-proto` 的真实类型（含事件流、含 enum with payload）成功生成一次 Swift 与 Kotlin 绑定并编译通过。**

平台顺序调整见 [ADR-0002](0002-android-first.md)：G1 阶段的绑定验证以 **Kotlin/AAR 为必过项**，Swift/XCFramework 为「生成成功即可，不要求接入 App」。

---

## 事实性修正（核对官方文档后发现，随本 ADR 一并生效）

### C-1 iroh 1.0 的类型改名

`REQUIREMENTS.md` §6.2 使用「NodeId(Ed25519 公钥)」的说法。iroh 1.0（当前 1.0.3）已改名：

| 0.x | 1.0 |
|---|---|
| `NodeId` | `EndpointId` |
| `NodeAddr` | `EndpointAddr` |
| `Connection::conn_type()` | 已删除。改用 `Connection::paths()` + `Path::is_ip()` / `is_relay()` / `is_selected()` |

**身份即公钥这一性质不变**，配对设计（§6.2）不受影响，仅术语需要更新。

**对实现方的强制要求**：训练数据中绝大多数 iroh 示例为 0.x，直接照抄必然编译失败。涉及 iroh 的任务卡一律要求先核对 <https://docs.rs/iroh/latest/iroh/>。

### C-2 局域网发现改用 iroh 官方 crate

`REQUIREMENTS.md` §3 写的是「mDNS（`mdns-sd`）」。iroh 官方已提供 `iroh-mdns-address-lookup`，与 `Endpoint` 直接集成，发现到的地址自动进入连接候选。

**决策**：改用 `iroh-mdns-address-lookup`，不自己接 `mdns-sd`。理由：自己接需要手工把发现结果喂回 iroh，等于重写官方已有逻辑，且容易与 iroh 的地址选择策略打架。

### C-3 Claude Code 的 ACP 适配器已改名

见 [ADR-0004](0004-acp-version-pinning.md)。

---

## 后果

- `REQUIREMENTS.md` §3、§6.2 按 C-1 / C-2 更新术语与选型
- G1 的 DoD 增加「UniFFI 绑定生成 + 编译通过」硬性条目
- 所有涉及 iroh 的任务卡必须附「核对 docs.rs，不许用 0.x API」的约束

## 影响的门禁

- **G0**：不变（选型本身不因 G0 结果改变，改变的是 relay 是否必做）
- **G1**：新增 UniFFI 绑定验证条目
