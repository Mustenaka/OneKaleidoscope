# OneKaleidoscope 当前状态

> 生效：2026-08-10
> 进度基线：T-108 Android emulator implementation complete；提交 SHA 与 CI 链接见
> [T-108 门禁结果](gates/T-108-result.md)。

## 1. 当前结论

**R0、R1、R2 已完成；R3 的 Android emulator 实现已完成，实体 arm64 设备门禁待执行。**

已经跑通并持久化验证的 PC 端纵切是：

```text
真实 Codex app-server → 钉定 decoder → reducer → canonical state
  → durable log → projection → Rust 诊断客户端
```

Agent 数据只来自公开结构化协议；PTY/TUI/ANSI 抓屏仍被禁止。

R3 的三条旧前置已全部解除：

- G-R1-1：Swift/Kotlin UniFFI callback、object、async、throwing 编译门禁由 T-102 解除；
- P-1：T-103 / ADR-0018 已用 `AttentionAnswerSource` 区分本地命令与外部观察；
- D-B1：T-104 / ADR-0019 已让真实 runtime acceptance 驱动 `LiveControl` / `Controlling`。

R3 开工审计又发现三个必须先闭合的合同缺口：

1. 多个 projection 共享 canonical stream cursor，projection 改变时 cursor 可能不前进；
2. UACP 把配对/分帧留到 R4，但 R3 本身要求安全 LAN 配对和重连；
3. 手机 prompt/free-form 没有安全的敏感正文写入入口，远端也不能被允许伪造 `Actor`。

T-106 已以 UACP `0.3.0`、投影独立游标和 TRANSPORT `0.1.0` 关闭以上缺口。
[T-107](tasks/T-107.md) 已通过；[T-108](tasks/T-108.md) 已完成 API 35 emulator
实现与真实 Codex LAN 纵切，但按任务卡约束，在实体 arm64 的 Wi-Fi、OEM 后台与
hardware-backed Keystore 证据登记前，R3 不得标记最终通过。

## 2. 已完成

| 里程碑 / 任务 | 结果 |
|---|---|
| R0 文档重新定基线 | 通过，[R0 结果](gates/R0-result.md) |
| R1 canonical 合同与 UniFFI 最小面 | 通过；原携带项已由 T-102 结清 |
| R2 Codex 本地纵切 / T-100 | 真实流式 turn、审批、队列、durable reload 通过 |
| T-102 | macOS Swift 与 Ubuntu Kotlin UniFFI consumer 编译通过 |
| 仓库收敛 | 完成；旧分支/主树分裂不再是活动状态 |
| T-105 | Codex `0.147.0` required-surface 审查完成；`0.146.0..=0.147.0` 证据闭合，Schema drift 恢复 |
| T-103 | UACP `0.2.0` Attention provenance、旧 `0.1` 日志 fail-loud |
| T-104 | `AcceptedByRuntime` → `LiveControl` → `Controlling`，含跨 runtime/session 防伪造守卫 |
| T-106 | UACP `0.3.0` mobile contract、projection 独立游标、可信 command/content ingress、TRANSPORT `0.1.0` |
| T-107 | pinned TLS LAN Broker、projection journal、可信 mobile ingress、Rust `MobileClient`、真实 Codex 冷重连纵切；两套三平台 CI 全绿 |
| T-108 | Android Compose 产品 App、Keystore/加密凭据、双 ABI UniFFI、七类真实 projection、错误 pin/冷启游标恢复/吊销 emulator 纵切；实体设备门禁待执行 |

## 3. 当前可用能力

| 模块 | 当前能力 | 产品边界 |
|---|---|---|
| `kaleido-state` | canonical state、内容寻址存储、八类 projection builder、持久 projection journal、device command outbox | projection journal 尚无物理 checkpoint/compaction |
| `kaleido-adapter-codex` | 真实 Codex JSON-RPC 会话、流式输出、file-change approval、prompt runtime ack | 尚无 interrupt/真实 steer delivery |
| `kaleido-hostd` | 常驻 Codex runtime、pinned TLS 配对/认证、订阅、content/command gateway、断线恢复 | 仅 LAN；公网 relay/push 在 R4 |
| `kaleido-core` | 产品级 `MobileClient`、pair/connect/reconnect/subscribe/command/content、last-good cache、移动端能力/正文 helper | iOS 产品接入仍在 R8 |
| Android | Compose Project/Session/Transcript/Live/Queue/Attention/Capability，Keystore P-256、加密凭据、离线缓存与 cursor resume | 实体 arm64/Wi-Fi/OEM 后台门禁待执行；无公网 relay/push |
| iOS | Swift 绑定可生成并编译 | 尚无产品 App，归 R8 |

因此当前已经是可在 API 35 emulator 上操作的 Android LAN 纵切；它仍不是 R3 最终成品，
因为实体手机门禁与公网能力均未完成。

## 4. R3 执行顺序

| 顺序 | 任务 | 交付 |
|---|---|---|
| 1 | [T-106](tasks/T-106.md) | UACP 0.3 projection cursor、可信 mobile command/content ingress、TRANSPORT 0.1 安全合同 |
| 2 | [T-107](tasks/T-107.md) | projection journal、ProjectIndex、pinned TLS 配对、hostd broker、Rust mobile core |
| 3 | [T-108](tasks/T-108.md) | Android Compose 项目/会话/实时/队列/Attention、冷启与断线恢复 |

下一步不是继续扩大 Android 功能面，而是在实体 arm64 Android 上执行 R3 最终门禁并登记
Wi-Fi、后台恢复与 hardware-backed Keystore 结果。

## 5. 尚未完成的后续里程碑

| 里程碑 | 内容 |
|---|---|
| R4 | Ubuntu rendezvous/relay、跨公网 E2EE、推送、活进程树终止 |
| R5 | OpenCode、Claude Broker 管理会话；先结清 OpenCode D-B11 |
| R6 | 跨 Agent workflow 执行器与手机 WorkflowBoard |
| R7 | 三家 CLI/GUI 六格真实验收 |
| R8 | iOS 产品对齐 |
| R9 | 路径安全、代码预览、Git、打包与发布硬化 |

## 6. 仍携带的问题

| ID | 内容 | 结清点 |
|---|---|---|
| D-B2 | 活进程树终止尚无真实存活子树测试 | R4 |
| D-B6 / D-B7 | Unix 反斜杠与 macOS `/var` 符号链接路径规则 | R9 |
| D-B8 | `<HOME>` / `<SANDBOX>` 脱敏标签精度 | R4 |
| D-B11 | OpenCode 实机与快照版本对齐 | R5 |
| P-2 | Codex approval 没有真实过期时间 | UI 必须诚实显示无过期时间 |

上游仍未提供稳定合同的 Codex Desktop / Claude 原生 GUI 第三方实时附着归 R7；
这不是“通过”，但不阻塞 Broker 管理会话的 R3～R6。

## 7. 文档优先级

冲突时依次以 `REQUIREMENTS.md`、生效 ADR、`PROTOCOL.md` + `kaleido-proto`、
`TRANSPORT.md`、`ARCHITECTURE.md`、本文与 `MILESTONES.md`、活动任务卡为准。
冻结任务和历史评审只能证明过去事实，不能重新定义当前范围。
