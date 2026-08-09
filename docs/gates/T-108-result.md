# T-108 门禁结果：Android 局域网纵切

> 日期：2026-08-10
> 状态：**emulator implementation complete；实体 arm64 设备门禁待执行**

## 1. 实现结论

T-108 已把 T-107 的安全 LAN Broker 与 Rust mobile core 投影为第一个 Android 产品纵切：

```text
真实 Codex structured JSON-RPC
  → hostd pinned TLS 1.3 LAN Broker
  → kaleido-core / UniFFI
  → Android Keystore + encrypted credential vault + no-backup cursor cache
  → Compose Project / Session / Transcript / Live / Queue / Attention / Capability
```

- `core-android` 固定构建 `arm64-v8a` 与 `x86_64`，生成 UniFFI Kotlin 并封装 AAR；
- App 使用 Material 3、Navigation、ViewModel/StateFlow，支持窄屏底部导航与宽屏双栏；
- P-256 私钥不可导出，Rust 在发送前严格复验 SPKI 与 DER 签名；配对凭据只进入加密偏好；
- projection/cursor cache 位于 no-backup 目录，正文不进入移动端持久缓存；
- capability、queue、attention、content digest 与命令合同由 Rust core 决策，Kotlin 不按 provider 名称推断。
- [ADR-0023](../adr/0023-projection-initial-sync-barrier.md) 用 ACK 后的有序 Ping/Pong 证明首批
  current/replay 已进入 Rust cache；exact-cursor resume 即使没有 callback 也能安全恢复 `Live`。

## 2. 本地门禁

```text
cargo xtask ci
exit 0
fmt-check / check-deps / lint-forbidden / clippy / workspace test: ok
fixtures-verify: 5 files, 220 records

Gradle 8.14
:core-android:verifyCoreAndroidAar
:app:assembleDebug
:app:assembleDebugAndroidTest
:app:testDebugUnitTest
:app:lintDebug
BUILD SUCCESSFUL
```

重点结果：

| 门禁 | 结果 |
|---|---:|
| `kaleido-core` | 22 passed |
| AAR ABI | `arm64-v8a`、`x86_64`，无其他 ABI |
| JVM tests | 12 passed |
| API 35 x86_64 instrumentation | 11 tests，0 failures；外部 real-LAN 用例在无 phase 的普通套件中明确不执行 |
| 无 host UniFFI native smoke | 1 passed，0 skipped；callback / async / error 均跨真实 `.so` |
| Android lint | 0 errors |

## 3. 真实 Codex + API 35 emulator 验收

使用本机已登录的 native `codex-cli 0.146.0`、生产 `CodexLanHost`、API 35 x86_64
emulator 与 APK 内真实 `libkaleido_core.so` 完成：

1. 错误 SPKI pin 在保存凭据前被拒绝；
2. 一次性 QR 配对、exporter-bound challenge 认证与 TLS 订阅成功；
3. ProjectIndex、SessionIndex、Transcript、LiveActivity、InputQueue、AttentionInbox、
   RuntimeCapability 七类真实 projection 均通过 Android 映射；无工具场景的 Attention 明确为空，
   没有伪造审批；
4. 经产品 `MobileRepository` 选择 project/session，并按真实能力发送 prompt 或入队；
5. 外部执行 `adb force-stop com.onekaleidoscope`，新进程先展示明确的离线 cache，再连接；
6. 重连后的 ProjectIndex 游标与 force-stop 前完全相同，没有重放当前项或重复列表；
7. Host durable revoke 后再次认证，产品状态进入明确 `Revoked`，不是任意关闭。

四阶段纵切（wrong-pin → seed → force-stop/resume → revoked）结果：`1 passed; 0 failed`，
14.69 秒。临时 host 驱动测试验收后已删除，
没有把本机路径、配对 URI、secret 或 prompt 写入仓库。

最终订阅审核修复后又以独立 production `lan run` 复跑真实 Codex：seed 阶段
`OK (1 test)` / 19.528 秒；外部 `adb force-stop` 后的 resume 阶段使用硬断言要求七类 panel
全部从 `CachedOffline` 恢复为 `Live`，结果 `OK (1 test)` / 28.242 秒。host 进程、adb reverse
与临时数据目录验收后均已清理。

## 4. 可失败变异证据

以下实现或测试夹具均被实际改坏、观察目标测试变红，再恢复并复测：

1. 把 `CapabilityState::NotVerified` 当作可发送 prompt，Rust capability 测试失败；
2. 把 UI 队列 `Pending` 夹具改成 `DeliveredNewTurn`，Pending 诚实语义 instrumentation 失败；
3. 把 encrypted credential 写入退化为普通 SharedPreferences + Base64，vault 测试失败；
4. 删除 subscribe ACK 后的 Ping/Pong 同步屏障，首批 projection 尚未完成时 `subscribe` 提前失败，
   hostd 产品纵切测试变红；恢复后通过；
5. 把缓存 high-water 的 stale 判定改成 equal，排队旧 callback 防回滚测试变红；恢复后通过；
6. 把屏障后的 `CurrentFollows` cache 校验收紧回“必须等于 ack cursor”，合法 live-before-Pong
   竞态测试变红；恢复为连续验证后的 `cache >= current` 后通过；
7. T-107 延续的错误 pin、复用 pairing secret、缺少 TLS exporter、cursor gap 等安全变异保持全绿守卫。

## 5. 明确保留边界

- 当前真实 Codex 无工具场景没有产生 Attention；approve/deny/question 的 runtime option、free-form
  和 disabled 原因由 Compose 行为测试与 Rust 合同覆盖，未伪报真实 approval 已发生。
- `AcceptedLocally` 只显示为 Broker 已持久记录，不显示为 runtime 已接受；真实 runtime acceptance
  仍由结构化 `turn/start` response 驱动。
- `TurnSteer` 未验证时只允许诚实排队，不冒充已 steer；interrupt 尚无 provider runtime API。
- 实体 arm64 Android 的 Wi-Fi、OEM 后台限制与 hardware-backed Keystore 尚无设备证据，
  因此 T-108 只能标记 emulator implementation complete，R3 尚未最终通过。

## 6. CI

提交后的三平台 CI、Android CI 与 GitGuardian 链接在 PR 合并后补记；在它们全绿前不得把本卡
标记为仓库最终通过。
