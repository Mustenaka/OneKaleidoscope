# T-112 Claude Agent SDK evidence

> 状态：**active evidence ledger；只有真实失败路径，不是成功验收**
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
8 file(s), 368 record(s)
codex: 3, acp-claude: 1, opencode: 3,
claude-sidecar: 1 file / 6 records, authentication-failure-only: 1
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
4. typed provider assistant event：`error = authentication_failed`；
5. typed terminal result：`is_error = true`。

六条 frame 全部来自钉定 `0.3.226` 官方 SDK 的这次实际运行，经新版 bridge 当场转换；没有把旧
raw SDK DTO fixture 手工改写为理想流量。运行仍得到同一 OAuth 失败语义，所以没有新增成功声明。

运行环境 OAuth session 已过期且刷新失败。因此该 fixture 证明真实 bridge/SDK/session/auth failure
路径，**不证明**成功模型 turn、工具成功、permission、QuestionSet、interrupt 或 resume。
任何 reducer 单测 frame 都不能填补这个真实 provider 空缺。

配套 metadata 固定：

```json
{"capture":"real_provider","provider_version":"0.3.226","expected_outcome":"authentication_failure","acceptance_eligible":false}
```

统一 fixture verifier 现在扫描该 sidecar fixture/metadata；缺 metadata、错误版本/预期或把
`acceptance_eligible` 改成 true 都会失败；成功 result 变异、伪造 acceptance metadata、无效 JSON
与密钥泄漏拒绝路径也已验证，因而真实 auth failure 不能被伪装成 provider success。

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
| real failure fixture verification | **通过** | metadata `acceptance_eligible=false`，统一 verifier 覆盖 |
| provider-neutral bootstrap | **通过（本地真实进程）** | provisional Session 与 T-113 双 runtime clean shutdown |
| discovery implementation | **focused 测试/类型通过** | official `listSessions()` metadata；尚无独立真实 discovery recording |
| history read implementation | **focused 通过；provider 验收未通过** | official `getSessionMessages(sessionId, { dir, offset, limit<=100, includeSystemMessages:true })` 已接入；只接受同一精确 discovery cwd/session，非空结果才证明 `HistoryRead` |
| no-credential history probe | **无可验收结果** | 对隔离 toy project 与当前 worktree 分别运行 exact-dir `listSessions(includeWorktrees=false, limit=1)`，均返回 0 session / 0 message；没有把空结果冒充 read 成功 |
| discovered-session ownership | **focused 实现/测试通过** | `listSessions()` 结果为 UACP `ProviderManaged`：只表达公开 SDK 管理持久化与 structured list/resume，不冒充独立 CLI/GUI attach |
| successful streaming turn | **未通过** | 真实 run 因 OAuth 过期失败 |
| real permission allow/deny | **未通过** | callback 与 reducer 实现/测试不等于 provider 验收 |
| real `AskUserQuestion` | **未通过** | official typed mapping 已实现，真实触发缺失 |
| real interrupt | **未通过** | `activeQuery.interrupt()` 路径已实现，真实 receipt 缺失 |
| real resume/recovery | **未通过** | `listSessions`/`resume` 路径已实现，真实重启恢复缺失 |
| capability honesty | **部分通过** | provisional/auth failure 不提升未证能力；最终 Android 展示未验 |
| `cargo xtask ci` | **最新集成候选本地通过** | 全入口 exit 0；真实 Claude success 空缺仍阻止 T-112 complete |

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

随后仍需要在有效 Claude 认证环境、隔离 toy project 中补录并验证：successful streaming turn、tool、
permission allow/deny、真实 QuestionSet、interrupt 与 resume。随后补齐 crash/malformed/duplicate
acceptance 错误测试与变异记录。在此之前 T-112 与 R5 保持 active。
