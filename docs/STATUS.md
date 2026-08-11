# OneKaleidoscope 当前状态

> 生效：2026-08-11
> 进度基线：R3 / T-109 实体 arm64 Android 最终门禁通过；提交 SHA 与本机证据见
> [T-109 门禁结果](gates/T-109-result.md)。R4 功能实现已由 PR #11 进入 `main`，最终公网
> release/physical acceptance 由 T-115 独立承接；R5 正把旧基线上的 provider 实现集成到该主线。

## 1. 当前结论

**R0、R1、R2、R3 已完成；R4 功能实现已通过 [T-110](tasks/T-110.md) / PR #11
合并，release/physical acceptance 仍由 [T-115](tasks/T-115.md) 持续，R4 未标 completed；
R5 / T-111、T-113 仍为 active；T-112 Claude managed-session 与 T-114 共享合同已完成，
但不代表 R5 完成。**

已经跑通并持久化验证的 PC 端纵切是：

```text
真实 Codex app-server → 钉定 decoder → reducer → canonical state
  → durable log → projection → Rust 诊断客户端
```

Agent 数据只来自公开结构化协议；PTY/TUI/ANSI 抓屏仍被禁止。

R5 当前分支已经落地 OpenCode 生成型 REST/SSE adapter、Claude 官方 Agent SDK sidecar、
provider-neutral multi-runtime hostd 与 UACP `0.5.0` QuestionSet/scoped runtime ack 合同。
真实 OpenCode `1.18.16` question fixture 已通过，但最新 generated product live probe 暴露同版本
`/doc` 与 `/event` 的 timestamp 类型冲突及未声明 heartbeat，adapter fail-closed，实时/恢复与 D-B11
仍被阻塞；Claude SDK `0.3.226` 已有真实成功 fixture 与 live acceptance probe，覆盖成功 turn、
permission allow/deny、QuestionSet、interrupt、resume、discovery 与非空 history。最新 R4/R5 集成候选的 `cargo xtask ci` 已完整通过；
用隔离安装的精确 Codex `0.147.0` 跑 schema diff，288 个 JSON 文件面内/面外均 0 drift。Android
双 ABI/UniFFI clean AAR/APK/lint/JVM 与 API 35 instrumentation 已在 `fc896be` 通过；全量设备测试
18 completed / 0 failed，2 个实体专用门禁在模拟器阶段按设计 skipped。随后 `c929a85` 已在实体
arm64 Android + 真实 Wi-Fi 上闭合 Claude provisional 首轮 NewTurn、真实审批 decline、七投影、
断线重连与 force-stop 后 cursor 14→17 冷启恢复；没有使用 `adb reverse`。exact-commit 跨平台 CI
仍待完成，Codex/OpenCode 与三 provider 同驻实体总格仍 blocked。

R3 的三条旧前置已全部解除：

- G-R1-1：Swift/Kotlin UniFFI callback、object、async、throwing 编译门禁由 T-102 解除；
- P-1：T-103 / ADR-0018 已用 `AttentionAnswerSource` 区分本地命令与外部观察；
- D-B1：T-104 / ADR-0019 已让真实 runtime acceptance 驱动 `LiveControl` / `Controlling`。

R3 开工审计又发现三个必须先闭合的合同缺口：

1. 多个 projection 共享 canonical stream cursor，projection 改变时 cursor 可能不前进；
2. UACP 把配对/分帧留到 R4，但 R3 本身要求安全 LAN 配对和重连；
3. 手机 prompt/free-form 没有安全的敏感正文写入入口，远端也不能被允许伪造 `Actor`。

T-106 已以 UACP `0.3.0`、投影独立游标和 TRANSPORT `0.1.0` 关闭以上缺口。
[T-107](tasks/T-107.md) 与 [T-108](tasks/T-108.md) 已通过；[T-109](tasks/T-109.md)
又在实体 arm64 Android 上完成真实 Wi-Fi、hardware-backed Keystore、file-change
approval decline、90 秒 OEM 后台、外部 force-stop cursor 恢复与 durable revoke，R3
最终门禁闭合。

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
| T-108 / T-109 | Android Compose 产品 App、Keystore/加密凭据、双 ABI UniFFI、七类真实 projection；API 35 emulator 与实体 arm64 Wi-Fi/审批/后台/冷启游标恢复/吊销门禁通过 |
| T-110 / R4 implementation | 自有 iroh relay、跨公网 pinned TLS E2EE、FCM FID/outbox、网络/冷启 cursor 恢复、三平台进程树终止；PR #11 合并，最终实现 SHA `603dfd8` 自动化全绿 |

## 3. 当前可用能力

| 模块 | 当前能力 | 产品边界 |
|---|---|---|
| `kaleido-state` | canonical state、内容寻址存储、八类 projection builder、持久 projection journal、device command outbox | projection journal 尚无物理 checkpoint/compaction |
| `kaleido-adapter-codex` | 真实 Codex JSON-RPC 会话、流式输出、file-change approval、prompt runtime ack | 尚无真实 steer delivery |
| `kaleido-adapter-opencode`（R5 integration） | OpenAPI 生成类型；REST discovery/history；SSE live；v2 prompt admission；permission/question/abort/reconnect 实现 | `1.18.16` fixture、REST/resume/queue receipt 有真实证据；latest live 因 `/doc` timestamp number 与 `/event` string 冲突及未声明 heartbeat fail-closed；SSE 恢复明确非无损 |
| `kaleido-adapter-claude`（R5 integration） | 官方 SDK `0.3.226` typed sidecar；provisional Broker Session、`ProviderManaged` metadata discovery/resume、bounded history、stream、permission/QuestionSet、interrupt | 真实 fixture/live probe 与实体 Android 已覆盖首轮 queue、approval decline、force-stop resume；三 provider 总格由 T-113 继续 |
| `kaleido-hostd` | provider-neutral registry、canonical Resume alias、receipt-gated NewTurn queue pump、StructuredLanHost；R4 自托管 iroh presence/P2P/relay、durable remote revoke | Claude provisional 首轮队列实体通过；OpenCode + Claude 双 runtime 启停通过；三家真实会话同驻未验 |
| `kaleido-core` | 产品级 `MobileClient`、pair/connect/reconnect/subscribe/command/content、逐 key last-good cache、Resume/Interrupt action；R4 pinned control、custom relay、网络 epoch 与 durable FCM outbox | iOS 产品接入仍在 R8；公网实体恢复仍待 T-115 |
| Android | 共享 Compose Project/Session/Transcript/Live/Queue/Attention/Capability 与 QuestionSet/Resume/Interrupt；R4 FCM FID、后台 WorkManager 与网络切换回调 | Claude 实体 Wi-Fi 子格通过；蜂窝/真实 FCM 与 R5 三 provider 总格未验收 |
| iOS | Swift 绑定可生成并编译 | 尚无产品 App，归 R8 |

因此当前已经交付并在实体 arm64 Android 上验证了 R3 Android LAN 纵切。公网 relay、
蜂窝网络切换与推送是 R4 的独立范围，不再作为 R3 未完成项。

## 4. 当前执行顺序

| 顺序 | 任务 | 交付 |
|---|---|---|
| 1 | [T-106](tasks/T-106.md) | UACP 0.3 projection cursor、可信 mobile command/content ingress、TRANSPORT 0.1 安全合同 |
| 2 | [T-107](tasks/T-107.md) | projection journal、ProjectIndex、pinned TLS 配对、hostd broker、Rust mobile core |
| 3 | [T-108](tasks/T-108.md) | Android Compose 项目/会话/实时/队列/Attention、冷启与断线恢复 |
| 4 | [T-109](tasks/T-109.md) | 实体 arm64 Wi-Fi、硬件密钥、真实审批、OEM 后台、force-stop 与吊销最终门禁 |

当前活动任务是 [T-115](tasks/T-115.md)：只执行 R4 自有 Ubuntu、真实 FCM、蜂窝与
实体 arm64 release/physical gate；不继续扩展 R4 功能代码。

截至 2026-08-11，T-110 的实现已闭合 ADR-0024、REMOTE_CONTROL 0.1、
自有 iroh relay、复用 R3 pinned TLS 的公网数据面、FCM FID、逐 key cursor 恢复；本轮审核又
补齐 route-scoped push、lost-ack outbox、Ubuntu ack 后主动 relay 断链、致命 control 错误断链、
Windows Job Object 与真实 iroh server 集成门禁。PR #11 最终实现 SHA `603dfd8` 的本地
`cargo xtask ci`、Windows/macOS/Linux、Ubuntu relay integration、Android clean build/lint/JVM
与 API 35 instrumentation 全绿并已合并为 `f7f7b3b`。这是 implementation merged，不是 R4
completed：自有 Ubuntu 的 DNS/ACME/FCM 运行及实体 arm64 Android 蜂窝公网纵切尚未执行，
真实门禁继续保持 [pending](gates/T-110-evidence.md)，由 T-115 承接。

R5 独立执行序列仍为：T-114 QuestionSet 合同（已完成）→ T-111 OpenCode 生成 REST/SSE 与
D-B11 → T-112 Claude SDK managed session → T-113 multi-runtime/移动端/跨平台与实体 Android
总门禁。T-111 与 T-113 保持 active，T-112 已完成；真实证据分别见
[T-111](gates/T-111-evidence.md)、[T-112](gates/T-112-evidence.md)、
[T-113](gates/T-113-evidence.md) 和 [T-114](gates/T-114-evidence.md)。

## 5. 尚未完成的后续里程碑

| 里程碑 | 内容 |
|---|---|
| R4 release gate / [T-115](tasks/T-115.md) | 自有 Ubuntu、真实 FCM、蜂窝/实体 arm64 全纵切与零 canary 验收 |
| R5 / [T-111～T-114](tasks/README.md) | active；Claude managed-session 与实体 Android 子格已通过；OpenCode generated live/D-B11 被同版本合同漂移阻塞，三家同驻与 exact-commit CI 未验收 |
| R6 | 跨 Agent workflow 执行器与手机 WorkflowBoard |
| R7 | 三家 CLI/GUI 六格真实验收 |
| R8 | iOS 产品对齐 |
| R9 | 路径安全、代码预览、Git、打包与发布硬化 |

## 6. 仍携带的问题

| ID | 内容 | 结清点 |
|---|---|---|
| D-B6 / D-B7 | Unix 反斜杠与 macOS `/var` 符号链接路径规则 | R9 |
| D-B11 | snapshot、required surface、CLI 与 SDK 公开版本号均对齐 `1.18.16`，但真实 `/event` 违反同版本 `/doc`（prompt-admitted timestamp number/string 冲突，另有未声明 heartbeat）；生成型 adapter fail-closed，故当前分支也未结清 | T-111 |
| P-2 | Codex approval 没有真实过期时间 | UI 必须诚实显示无过期时间 |

上游仍未提供稳定合同的 Codex Desktop / Claude 原生 GUI 第三方实时附着归 R7；
这不是“通过”，但不阻塞 Broker 管理会话的 R3～R6。

## 7. 文档优先级

冲突时依次以 `REQUIREMENTS.md`、生效 ADR、`PROTOCOL.md` + `kaleido-proto`、
`TRANSPORT.md`、`ARCHITECTURE.md`、本文与 `MILESTONES.md`、活动任务卡为准。
冻结任务和历史评审只能证明过去事实，不能重新定义当前范围。
