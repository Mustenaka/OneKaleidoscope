# T-107 门禁结果：hostd LAN Broker 与 Rust mobile core

> 日期：2026-08-09
> 状态：**本地实现门禁通过，等待新 SHA 三平台 CI**

## 1. 实现结论

T-107 已形成不依赖 PTY/TUI 的 PC hostd ↔ Rust mobile core 生产纵切：

```text
Codex app-server structured JSON-RPC
  → reducer / canonical store / projection journal
  → pinned TLS 1.3 LAN Broker
  → authenticated MobileClient / durable last-good cache
```

- `kaleido-state`：八类闭合 projection builder、每 key 独立持久 journal/cursor、显式
  fanout、device-owned content、request digest 与 at-most-once command outbox。
- `kaleido-transport`：精确 SPKI pin、一次性 QR pairing、P-256 exporter-bound challenge、
  device revoke、闭合 frame/correlation、绝对阶段 deadline 与连接/尝试/订阅限额。
- `kaleido-hostd`：唯一 canonical writer、runtime supervisor、订阅 hub、content/command gateway、
  默认常驻至 Ctrl-C；listener ready 后才发布 `LanDirect`。
- `kaleido-core`：UniFFI `MobileClient`、pair/connect/reconnect、subscription callback、
  command/content API、严格 projection key/cursor/version cache。

## 2. 本地门禁

```text
cargo xtask ci
exit 0
fmt-check / check-deps / lint-forbidden / clippy / workspace test: ok
fixtures-verify: 5 files, 220 records

cargo ndk -t arm64-v8a -t x86_64 build -p kaleido-core
exit 0

Gradle 8.14 clean compileKotlin
BUILD SUCCESSFUL; 2 actionable tasks executed
```

最终重点计数：

| crate / gate | 结果 |
|---|---:|
| `kaleido-hostd` | 27 passed |
| `kaleido-state` | 58 passed（14 unit + 44 integration） |
| `kaleido-core` | 14 passed |
| `kaleido-transport` | 34 passed |
| `kaleido-proto` contract | 43 passed |
| Android ABI | arm64-v8a、x86_64 均通过 |
| Kotlin consumer | 通过 |
| Swift consumer | bindings 已生成；实际编译等待 macOS CI |

## 3. 真实验收

使用本机已登录的 native `codex-cli 0.146.0`、生产 `CodexLanHost` 与真实
`MobileClient` 完成 123.84 秒 smoke：

1. QR pair 与 exporter-bound challenge connect；
2. 订阅 ProjectIndex、SessionIndex、Transcript、AttentionInbox；
3. ContentWrite `Reply OK only. Do not use tools.`，随后远程 SubmitPrompt；
4. 初始 ack 为 `AcceptedLocally`，结构化 `turn/start` response 后才出现 runtime acceptance；
5. 真实 turn 到达 `Completed`，AttentionInbox 始终为空，明确没有等待应答项；
6. 记录 terminal cursor，断开并销毁客户端；
7. 新 `MobileClient` 冷启动 challenge reconnect，以相同 cursor 续订，300 ms 内无重复。

临时 smoke 测试在验收后删除，没有把本机路径、凭据或 prompt 写入仓库。

## 4. 可失败变异证据

以下实现均被实际改坏、观察到目标测试变红，再恢复并复测：

1. 接受错误 SPKI pin；
2. 允许复用已消费 pairing secret；
3. challenge transcript 不绑定 TLS exporter；
4. listener ready 后仍发布 Offline；
5. 既存 content digest 校验由 `len || digest` 错改为 `len && digest`；
6. 移除 mobile cache 的严格 cursor sequence 校验。

## 5. 安全与恢复结论

- unknown/revoked/wrong-signature 使用同一 challenge/失败粒度并受来源与全局尝试桶限制；
- TLS、两次 hello、auth、frame 各有绝对 deadline，slow drip 不能续命；
- revoke 与 Broker 业务入口无 TOCTOU，连接限额在 `AuthAccepted` 前执行；
- Ready 在 runtime 可路由时才 claim；Claimed 崩溃后保持 uncertain，绝不自动重发；
- 旧 ephemeral Session 的 Ready 命令不改写目标，durable `RuntimeUnavailable` 后完成 outbox；
- secret、正文、私钥、完整用户路径不进入 tracing、canonical/projection/outbox side files。

## 6. 明确保留边界

- projection journal 当前做逻辑 retention；没有版本化 checkpoint/floor marker 前不做物理截头，
  否则无法区分合法压缩与损坏 gap。
- late runtime rejection 没有独立 command-status projection；手机通过相关 session/turn/attention
  状态观察结果。若要展示离线恢复 rejection，需要后续 ADR 增加查询/投影面。
- 本卡不含 Android UI、R4 relay/push、真实 TurnSteer delivery 或其他 provider。

## 7. 待完成

- 新提交 SHA 的 Windows / Ubuntu / macOS CI；macOS 同时提供 Swift consumer 编译证据。
