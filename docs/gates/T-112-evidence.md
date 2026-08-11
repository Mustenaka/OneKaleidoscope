# T-112 Claude Agent SDK evidence

> 状态：**真实 SDK managed-session 验收通过；移动端总门禁仍由 T-113 承接**
> 集成基线：`origin/main@b54a12638cd044b277747d5fbc22627ad2adb016`
> 记录日期：2026-08-11

## 1. 钉定上游与 bridge

- `@anthropic-ai/claude-agent-sdk@0.3.226` 在 `package.json` 与 lockfile 精确钉定；
- bridge 使用官方 `query()`、`listSessions()`、`getSessionMessages()`、`activeQuery.interrupt()`、
  `canUseTool` 与官方 `AskUserQuestionInput` / `SDKMessage` / `SessionMessage` 类型；
- `SDKMessage` 顶层 union、system subtype 与 actionable content block 均由 TypeScript exhaustive
  switch 转为闭合自有 event；Rust 对 envelope、payload、event 与 block 做
  `deny_unknown_fields` 解码，不再接收整段上游 DTO；
- `npm run typecheck` 已通过；
- Rust 只解码 `onekaleidoscope.claude.sidecar` 闭合 frame，没有手写 Claude SDK DTO；
- raw session/message/tool/request ID 只在 adapter 私有状态与受控 fixture。

Claude sidecar 现在有独立硬门禁：

```text
cargo xtask claude-sidecar
  -> npm/npm.cmd ci --ignore-scripts
  -> npm/npm.cmd run typecheck
```

该 step 已进入 `cargo xtask ci`；三平台 workflow 使用 Node 22。最新 R4/R5 集成候选的完整本地
`cargo xtask ci` exit 0，包含 fmt、check-deps、lint-forbidden、clippy、Claude sidecar、workspace/
doc tests 与 fixtures verify。fixture 汇总为：

```text
8 file(s), 369 record(s)
codex: 3, acp-claude: 1, opencode: 3,
claude-sidecar: 1 file / 7 records, acceptance: 1, authentication-failure-only: 0
```

exact-commit Windows/macOS/Linux run 仍由 T-113 记录，不能用本地总门禁替代。

focused reducer 测试已覆盖 structured QuestionSet frame；bridge prompt queue 只有在官方 SDK query
消费 user message 后才发 `prompt_accepted`，adapter 再把它与本地 command、目标 canonical
Session 和唯一 RemoteCommand Turn 关联为 `PromptTurn`；interrupt receipt 使用
`SessionControl` 且不创建 Turn。不把 stdin/本地排队冒充 runtime acceptance。

## 2. 真实 SDK fixture

文件（2026-08-11 重新运行 `npm run record:real`，不是手工迁移）：
`crates/kaleido-adapter-claude/tests/fixtures/sandbox/real-sdk-simple-turn.jsonl`。

该 fixture 来自真实 `0.3.226` SDK managed session，包含：

1. sidecar `ready`（精确 cwd 与 nullable resume id）；
2. SDK 消费 prompt 后的 `prompt_accepted`；
3. 真实 `session_started` 与 typed SDK init event；
4. typed provider assistant event：非空 text block 且没有 error；
5. typed terminal result：`subtype = success`、`is_error = false`。

七条 frame 全部来自登录后的钉定 `0.3.226` 官方 SDK 实际运行，经 bridge 当场转换；没有把旧 raw
SDK DTO fixture 手工改写为理想流量。配套 live probe 还在隔离临时目录真实完成成功 turn、
QuestionSet 回答、permission allow、permission deny（并验证被拒文件没有创建）、interrupt receipt、
同一 session resume、精确目录 discovery 与非空 history；临时目录在进程退出后删除。

配套 metadata 固定：

```json
{"capture":"real_provider","provider_version":"0.3.226","expected_outcome":"simple_turn_success","acceptance_eligible":true}
```

统一 fixture verifier 现在扫描该 sidecar fixture/metadata；缺 metadata、错误版本/预期、把
`acceptance_eligible` 改成 false 或把 terminal success 变成 error 都会失败；无效 JSON 与密钥泄漏
拒绝路径也继续覆盖。实际把 verifier 的 `is_error == false` 判断反转后，acceptance 测试变红；恢复后通过。

## 3. provisional Session 证据

当前 live sidecar 的 `ready` 携带 `cwd`。adapter 在 Claude 尚未分配 raw session id 前创建稳定的：

```text
ownership = BrokerManaged
history_source = None
live_binding = NotBound(NeverStarted)
binding_handle = None
```

收到真实 `session_started` / `session_resumed` 后才给同一 canonical Session 加 provider binding
并提升相应 history/live 证据。provisional Session 只解决 hostd bootstrap/routing，不是 Claude 成功
会话或原生 CLI/GUI attach 证据。

旧 committed fixture 的 `ready` 没有 `cwd`，因此仍按历史 frame 解码但不会错误地创建 provisional
Session。

## 4. 当前验收矩阵

| 面 | 状态 | 证据/缺口 |
|---|---|---|
| official SDK typecheck / CI step | **通过（focused）** | pinned package/lockfile + `cargo xtask claude-sidecar`；已接入总 CI |
| real success fixture verification | **通过** | metadata `acceptance_eligible=true`，统一 verifier 覆盖 |
| provider-neutral bootstrap | **通过（本地真实进程）** | provisional Session 与 T-113 双 runtime clean shutdown |
| discovery implementation | **focused 测试/类型通过** | official `listSessions()` metadata；尚无独立真实 discovery recording |
| history read implementation | **focused 通过；provider 验收未通过** | official `getSessionMessages(sessionId, { dir, offset, limit<=100, includeSystemMessages:true })` 已接入；只接受同一精确 discovery cwd/session，非空结果才证明 `HistoryRead` |
| real history read | **通过** | live probe 对刚创建的同一 exact-dir/session 执行 list + bounded history，得到非空官方消息页 |
| discovered-session ownership | **focused 实现/测试通过** | `listSessions()` 结果为 UACP `ProviderManaged`：只表达公开 SDK 管理持久化与 structured list/resume，不冒充独立 CLI/GUI attach |
| successful streaming turn | **通过** | committed real fixture + live probe terminal success |
| real permission allow/deny | **通过** | 两条真实 tool request；allow 后隔离文件存在，deny 后目标文件不存在 |
| real `AskUserQuestion` | **通过** | official typed QuestionSet request/result，单题单选回答后 turn success |
| real interrupt | **通过** | `activeQuery.interrupt()` 返回结构化 receipt |
| real resume/recovery | **通过** | 新 sidecar process 以同一 session id resume 并再次完成 turn |
| capability honesty | **通过（provider 面）** | capability 只随真实 typed evidence 提升；最终 Android 展示仍由 T-113 验收 |
| `cargo xtask ci` | **待本分支最终 SHA 一次性总门禁** | focused provider/verifier 测试已通过，不重复冒充总门禁 |

## 5. 解除条件

本地无需凭据的实现已接入 typed `getSessionMessages()`、严格 frame、start retry/error cleanup、
non-eager resume、close acknowledgement 与跨平台进程树清理。focused 门禁为：

```text
npm run typecheck                                      exit 0
cargo clippy -p kaleido-adapter-claude --all-targets -- -D warnings  exit 0
cargo test -p kaleido-adapter-claude                   10 passed
```

方向守卫变异验证：临时把 `BridgeToHost` 拒绝条件反转后，
`host_direction_and_changed_session_identity_are_rejected_before_projection` 实际失败；恢复后通过。

本轮真实 Provider 命令为 `npm run record:real` 与 `npm run probe:real-acceptance`；后者输出
`result=pass`，上述十项 evidence 全为 true。T-112 的 SDK managed-session 范围已闭合；三 Provider
同驻、实体 Android 与 exact-commit CI 继续由 T-113 负责，OpenCode 上游漂移仍由 T-111 阻塞。
