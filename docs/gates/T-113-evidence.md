# T-113 multi-provider / mobile gate evidence

> 状态：**active；composition bootstrap 通过，最终门禁未通过**
> 集成基线：`origin/main@b54a12638cd044b277747d5fbc22627ad2adb016`
> 记录日期：2026-08-11

## 1. provider-neutral host composition

`kaleido-hostd` 已增加 `RuntimeBootstrapFactory` / `StructuredLanHost`，在同一 broker store、
projection journal 与 LAN gateway 下注册多个 provider runtime。SubmitPrompt/RespondAttention/
Interrupt command 可按 canonical Session route 到 owner runtime；discovery 得到的 canonical
Session 也可通过 `ResumeSession` alias 回到同一 runtime actor，raw provider ID 不离开 adapter。
所有 provider 继续产出同一 `StateEffect` 与八类 projection。

本地双 runtime 运行使用真实 OpenCode `1.18.16` pure server 和真实 Claude Agent SDK
`0.3.226` sidecar：

```text
<repo>/target/debug/kaleido-hostd lan run
  --providers opencode,claude
  --opencode-url http://127.0.0.1:41816
  --node-executable <node.exe>
  --claude-bridge <repo>/crates/kaleido-adapter-claude/bridge/index.ts
  --project-root <repo>/target/r5-live-opencode/project
  --data-dir <repo>/target/r5-hostd-both-pass
  --bind 127.0.0.1:0 --serve-secs 1 --timeout-secs 5 --no-print-pairing
```

结果：exit 0，摘要：

```text
LAN sessions ses_aad3bcfecbad149a,ses_0177cdb6084b62b9 stopped cleanly
```

这条历史命令只证明 OpenCode + Claude 两个 runtime 同驻、bootstrap 与 clean shutdown。Claude
随后已用独立真实 SDK probe 闭合成功 turn/人工交互/恢复，但 Codex 未参加同一运行，因此仍不能称为
三家真实会话同驻。

宿主编排现有 9 条 deterministic test 覆盖多 worker、scoped route、interrupt、discovered
Session resume alias、Idle/NewTurn structured receipt delivery、无 receipt/Steer 保持 Pending、
drain/reconnect lifecycle report、provider-neutral R4 remote path 与启动失败回滚；`kaleido-hostd`
focused clippy、9/9 R5 tests 通过。切断 InterruptTurn route 的变异曾实际变红，恢复后通过。这些测试使用明确标注的
test double，只证明宿主逻辑，不冒充 provider fixture 或实体运行。

## 2. 共享合同与移动消费面

- UACP QuestionSet、mandatory scoped runtime acknowledgement（`PromptTurn` / `SessionControl`）
  与 provider-neutral interrupt route 已进入共享 proto/state/core/transport；
- Android Compose 复用同一个 Attention UI，逐题编辑并一次提交，不按 provider 名称分支；
- Kotlin/Swift probe 已真实编译；
- Android main/androidTest source 编译与 unit tests 已通过；
- 当前没有 R5 实体 Android 运行，所以以上编译结果不能冒充设备纵切。

最新 focused 结果：proto 44/44、core 42/42、hostd R5 9/9，四个受影响 Rust crate 的 all-target
clippy `-D warnings` 通过。最新集成候选的 Android 增量命令包含 `:app:compileDebugKotlin`、
`:app:compileDebugAndroidTestKotlin` 与 `:app:testDebugUnitTest`，结果 `BUILD SUCCESSFUL`，并实际构建
`arm64-v8a` + `x86_64` Rust/UniFFI。

稳定候选 `fc896be` 随后执行一次 clean Android 总门禁：

```text
clean :core-android:verifyCoreAndroidAar :app:assembleDebug
:app:assembleDebugAndroidTest :app:testDebugUnitTest :app:lintDebug
BUILD SUCCESSFUL; 157 actionable tasks
arm64-v8a + x86_64 release Rust/UniFFI
```

本机 `Medium_Phone_API_35` AVD 的 native smoke 为 1/1；全量
`:app:connectedDebugAndroidTest` 为 18 completed / 0 failed。`RealLanBridgeTest` 与
`PhysicalDeviceSecurityGateTest` 两个实体专用测试按设计 skipped，没有计为通过。测试结束后 emulator
已关闭，`adb devices` 为空；因此以上是 API 35 自动化证据，不是实体 arm64/provider 纵切证据。

## 3. 本地总门禁

最新 R4/R5 集成候选的完整 `cargo xtask ci` exit 0，包含 fmt、check-deps、lint-forbidden、clippy、
`claude-sidecar`、workspace test 与 fixtures verify。fixture summary：

```text
8 file(s), 369 record(s)
codex: 3, acp-claude: 1, opencode: 3,
claude-sidecar: 1 file / 7 records, acceptance: 1, authentication-failure-only: 0
```

隔离安装精确 Codex `0.147.0` 后，`cargo xtask schema diff` exit 0：Codex `0.147.0`、OpenCode `1.18.16`、ACP schema `1.18.0`，
共 288 JSON files，required-surface 内外 0 drift。该静态结果不能发现 T-111 真实 `/event` 违反
同版本 `/doc` 的 payload drift；Claude 成功证据来自真实 SDK probe，不由 schema diff 推断。

首次 clean workspace test 编译遇到一次 Windows rustc `STATUS_STACK_BUFFER_OVERRUN`；同一候选树、
同一 clean target 的 hostd all-targets 定向复跑全部通过，随后完整 `cargo xtask ci` 复跑 exit 0。
该工具链瞬时失败没有被记成绿，也没有通过删除/放宽测试处理。schema diff 还发现并修复了两项
门禁自身问题：taskkill 与精确后代句柄竞态不再把已退出后代误报为 AccessDenied，而真正未退出仍
fail-closed；surface-history 以 tool/version/required-surface entry set 区分经审查的 surface 扩展，
同版本同 entry set 摘要冲突仍硬失败。

queue pump 每轮只取 Idle Session 的第一条 `NewTurn`：先 durable `Submitting`，只有 provider
structured receipt 后才写 Turn + `DeliveredAsNewTurn`；崩溃后的 uncertain entry 不自动重发。
Steer 与无 receipt 路径继续 `Pending`。OpenCode v2 queue admission 证明 `QueueWrite`，但新增
canonical pump 尚需最终真实 provider 重跑；Claude 本地 SDK input queue 不自动证明 steer。

## 4. 总门禁

| 门禁 | 状态 | 说明 |
|---|---|---|
| OpenCode + Claude dual runtime bootstrap | **通过（本地）** | 历史同驻命令 + Claude 独立真实 acceptance probe |
| Codex + OpenCode + Claude 三家真实 Session 同驻 | **未通过** | OpenCode stable live contract drift；没有三家同一运行证据 |
| 三家 streaming/history/waiting-human/reconnect | **未通过** | Claude 已通过；OpenCode stable live/reconnect 仍 fail-closed |
| Resume / queue / steer / interrupt contract | **focused 实现/测试通过** | canonical Resume alias、NewTurn receipt delivery、Steer Pending；需真实 provider + Android 纵切 |
| host/provider crash 与 mobile cursor resume | **未通过 R5 最终纵切** | R3 Codex LAN 历史证据不能自动覆盖新 provider |
| `cargo xtask ci` | **最新集成候选本地通过** | 全入口 exit 0；不替代 exact-commit remote CI |
| `cargo xtask schema diff` | **最新集成候选本地通过** | 精确 Codex `0.147.0`；288 JSON files、in/out surface 0 drift；OpenCode live contract drift 仍另行阻塞 |
| Android clean build/lint/API 35 | **`fc896be` 本地通过** | 双 ABI AAR/APK/JVM/lint；native smoke 1/1；全量 18 completed / 0 failed，2 physical-only skipped |
| Windows/macOS/Linux CI + Android CI | **workflow 已接 Node 22/Claude sidecar gate；无 exact commit run** | 仍不能标绿 |
| 实体 arm64 Android + 真实 Wi-Fi 三家纵切 | **未执行** | 禁止用 emulator/mock/`adb reverse` 替代 |
| R4 合并后蜂窝/relay 重跑 | **外部验收 pending** | R4 实现已进 `main`；T-115 的实体蜂窝/relay 门禁仍独立 pending |

## 5. 已知本地实现阻塞

- `CapabilityProbe` 目前只能表达 `Supported` / `NotVerified`，尚未把断线、OAuth auth failure 与
  明确 upstream block 映射为 `UnavailableOnThisConnection` / `AuthenticationRequired` /
  `UpstreamBlocked`；
- Claude typed history read 已用真实新建 session 得到非空官方消息页；permission allow/deny、QuestionSet、
  interrupt 与新进程 resume 也已通过。discovered Session 使用 `ProviderManaged`，不冒充原生 CLI/GUI ownership；
- OpenCode SSE 已按 event session 精确路由并按 event id 去重，但 provider reconnect 仍明确
  `lossless_replay = false`；真实 gap/forced-disconnect 证据缺失。SSE reader 已与 command dispatch
  解耦，Attention 只有精确 typed SSE 回执才完成；真实 OpenCode `/doc`/`/event` 漂移仍 fail-closed。

## 6. 完成条件

T-113 只有在 T-111 上游空缺闭合、三家同驻纵切、`cargo xtask ci`、跨平台
CI exact commit runs 与实体 arm64 Android 真实 Wi-Fi 纵切全部通过后才能完成。R4 合并后还必须
在最终 SHA 重跑蜂窝/relay；当前 LAN 证据不等于公网恢复。

## 7. 零构建外部预检

`scripts/r5-provider-acceptance-preflight.ps1` 是最终门禁的第一步。它只读取环境，不生成 APK、
不编译 workspace，也不记录凭据、设备序列号或完整用户路径。预检覆盖：

- candidate 包含 `origin/main` 且工作树 clean；
- 可运行的 native Codex、OpenCode CLI 与 schema 精确版本一致；
- Claude CLI 已登录、bridge 仍钉定官方 SDK、Node 至少为 22；
- 恰好一台非模拟器 arm64 Android、真实 Wi-Fi、到 PC 同链路直达且无 `adb reverse`；若系统叠加 VPN，
  只有目标 PC 地址明确经 `wlan0` 且无网关转发时才接受为 LAN bypass。

2026-08-11 在提交 `3d3bca5` 上实际运行完整预检 exit 0：Codex `0.147.0`、OpenCode/schema
`1.18.16`、Claude SDK `0.3.226`、Node `22.13.1`、Claude 登录、实体 arm64、同链路 Wi-Fi 与空
`adb reverse` 全部成立。手机的默认网络含 VPN transport，但对目标 PC 的实际 route 是 `wlan0`
直连且没有 `via` 网关，因此记录为 `wifi-direct-vpn-bypass`，没有通过删除 VPN 检查消绿。
