# ADR-0008: 版本兼容模型 —— 从「钉死一点」改为「必需面 + 观测记录」

- 状态：**已接受**（2026-07-30）
- 决策人：项目负责人
- 起草：项目主管
- 取代：[ADR-0004](0004-acp-version-pinning.md) P-1/P-2 的「钉死确切版本」执行方式（版本记录本身保留）
- 影响：`schemas/`、`xtask schema`、`docs/UPSTREAM.md`、`docs/REQUIREMENTS.md` §9 R-4

---

## 背景：钉死一点的模型已经三次挡住我们自己

| 时间 | 现象 |
|---|---|
| T-006 | Codex `0.144.6 → 0.146.0`，`schema diff` 报 **2380 处**漂移（+548 / ~1598 / −234） |
| T-010 | Codex 沙箱内无法启动 `.CMD`，`schema diff` 退出码 2 |
| T-010 后 | 负责人本机：`tool opencode has version 1.18.9, expected 1.18.8` → **退出码 2，拒绝运行** |

最后这一条是设计上的自相矛盾：

> **一个漂移检测器，因为「安装的版本与快照不一致」而拒绝运行。**
> 而「版本不一致」正是它唯一需要检测的东西。

根因是 T-003 的任务卡把「工具缺失」与「版本不符」都归进了同一个失败类别。
更根本的是，`REQUIREMENTS.md` §4 与 ADR-0004 采用的是「钉死一个确切版本」模型，
而三家 agent 的实际发布节奏是周级甚至日级，且用户环境会自动升级。

**追着钉版本是条治不好的路。**

---

## 决策

### D-1 把被混为一谈的三个概念分开

| 概念 | 含义 | 可变性 |
|---|---|---|
| **snapshot version** | `schemas/` 下这份快照是从哪个版本抓的 | 不可变的历史记录 |
| **observed version** | 此刻机器上实际装的是哪个版本 | 随时会变，**不受我们控制** |
| **supported range** | 我们声称能配合工作的版本范围 | 由证据决定，可扩可缩 |

当前实现只有 snapshot version，并把它当作 observed version 的**准入门槛**。这是缺陷本身。

### D-2 版本不一致是数据，不是错误

**`schema diff` 永不因版本不符而拒绝运行。** 它必须始终对「实际装的那个版本」执行比较，
并在输出中同时报告 snapshot version 与 observed version。

只有**工具真的不存在或无法执行**才是错误。

### D-3 漂移按「必需面」分区 —— 这是本 ADR 的核心

2380 处漂移之所以无从下手，是因为它把「我们依赖的东西变了」和
「我们根本不用的功能变了」混在一起报。

Codex 的 275 个 schema 里，UACP 实际依赖的只是其中一小部分：thread / turn / item 生命周期、
两类审批请求、JSON-RPC 信封。realtime、plugin、marketplace、apps、hooks 这些我们一个都不碰。

**引入 `schemas/required-surface.toml`**：声明式列出 UACP 依赖的方法、通知与类型。
`schema diff` 计算完漂移后**分区报告**：

| 分区 | 处置 |
|---|---|
| **面内漂移**（required surface 之内） | **失败**。这是真警报，需要人看 |
| **面外漂移** | 只报数量与摘要，**不失败** |

必需面的内容由 `PROTOCOL.md` 推导，**不许由「哪个没漂移」倒推**（同 ADR-0005 D-4 的防滥用条款）。

### D-4 退出码语义重新定义

| 码 | 含义 |
|---|---|
| `0` | 面内无漂移。**版本不同但面内一致也算 0**，并打印提示 —— 这是补丁升级的常态 |
| `1` | **面内**有漂移。输出变化的 JSON 路径清单 |
| `2` | 工具不存在或无法执行（**不含**版本不符） |

### D-5 运行时能力优先于版本号

这条项目里已有先例（ADR-0003 `capabilities()`、ADR-0006 五层发现、ADR-0007 `caps.elicitation`），
此处推广为通则：

> **adapter 判断「能做什么」的依据是握手时协商到的能力与实际 schema，
> 不是版本号字符串比较。**

三家都提供了运行时自述：Codex `initialize` 返回 capabilities；ACP 返回 `agentCapabilities`；
OpenCode 的 `/doc` 可实时抓取。**禁止出现 `if version >= X` 式的功能开关。**

### D-6 纵向漂移记录

`schemas/` 只为**当前 snapshot version** 保留完整快照。
但每次观测到新版本时，追加一条**必需面摘要**（每个必需面条目的哈希）到
`schemas/surface-history.jsonl`。

代价极小，收益是把 R-4 从「每次都重新震惊一次」变成一份可查询的上游变更速率数据。

### D-7 supported range 是声明，不是门闩

`docs/UPSTREAM.md` 记录每个工具的 supported range 与判定依据（哪次实测通过了）。
它用于**文档与告警**，不用于**阻止执行**。

超出 range 时：正常运行 + 打印醒目提示「未验证的版本」。

---

## 被否决的方案

| 方案 | 否决理由 |
|---|---|
| 继续追着钉死确切版本 | 已三次挡住我们自己；上游节奏是周级，我们追不动 |
| 完全不钉版本 | 失去可复现性，也失去 R-4 的检测基准 |
| 按 semver 范围硬判并拒绝范围外版本 | 换个形式的同一个错误。三家的版本号语义并不严格遵循 semver（Codex 两个补丁位改了 2380 处） |
| 每个版本都存完整快照 | 275 文件 × N 版本，仓库迅速膨胀，且绝大多数内容我们不关心。D-6 的摘要方案成本低得多 |
| 把漂移全部降级为警告 | 面内漂移是真会让 adapter 崩的东西，必须失败 |

---

## 后果

- `xtask schema diff` 行为改变（D-2/D-3/D-4），实现见 T-011
- 新增 `schemas/required-surface.toml`（provisional 版由 T-011 建立，**定稿由主管随 `PROTOCOL.md` 一并产出**）
- 新增 `schemas/surface-history.jsonl`
- `docs/UPSTREAM.md` 增加 supported range 一节
- `REQUIREMENTS.md` §9 R-4 的缓解措施更新

## 影响的门禁

- **G1**：`required-surface.toml` 必须与 `PROTOCOL.md` 的依赖面逐条对齐，主管审核项
- **G2**：三家 adapter 不得出现按版本号分支的功能开关（D-5），审核项
- **G8**：CI 的每日漂移检查按新退出码语义接线
