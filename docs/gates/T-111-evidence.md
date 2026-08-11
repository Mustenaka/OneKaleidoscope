# T-111 OpenCode / D-B11 evidence

> 状态：**active evidence ledger，不是完成证明**
> 集成基线：`origin/main@b54a12638cd044b277747d5fbc22627ad2adb016`
> 更新日期：2026-08-11

## 1. D-B11 版本结论

| 版本 | schema / required surface | 真实 provider 证据 | 结论 |
|---|---|---|---|
| `1.18.8` | 原 T-003/T-004 快照与 surface-history 基线 | T-004/T-006 真实 REST+SSE session/event traffic | 历史回归证据；不再声称当前支持下界 |
| `1.18.11` | 不作为当前全量快照 | `tests/fixtures/opencode/01-simple-turn.jsonl`，53 records，真实 simple streaming turn | 历史 runtime 回归；不是当前支持声明或 `1.18.16` question 证据 |
| `1.18.15` | `schemas/surface-history.jsonl` 有 schema observation | 本卡没有对应真实 runtime fixture | 只作 drift ledger，不单独声称运行验收 |
| `1.18.16` | 当前原样 `/doc` 快照；required surface 增加 create/prompt-v2/question/live event 等本纵切必要面 | `09-elicitation` 真实 fixture + 本地 `opencode serve --pure` product live probe | snapshot 与 runtime 版本相同，但实时 payload 违反该 `/doc`；D-B11 仍被阻塞 |

`schemas/required-surface.toml` 记录的 warning range 是 exact `=1.18.16`。它不是 feature
switch；adapter 只按当前连接的结构化成功/流量证明能力。早期 fixture 保留为回归资产，但
1.18.9～1.18.14 缺少逐版本证据，因此不声明连续范围。范围外版本仍允许 `schema diff` 并显示
unverified warning。2026-08-11 重新执行 `opencode --version`、`npm view opencode-ai version`
与 `npm view @opencode-ai/sdk version`，三者仍均返回 `1.18.16`，因此下述漂移不能解释为
CLI/server 与 SDK 的公开版本号不一致。

同日又在隔离安装中重新审计 `1.18.11`，排除“降级即可支持”的假设：精确
`cargo xtask schema diff` 相对当前快照得到 required surface 内外均 `0 drift`，但真实 Windows
server 对明确隔离目录的 `/project/current` scope 返回盘符根目录。产品 adapter 因
`ScopeMismatch` 在 discovery 阶段 fail-closed；没有放宽目录比较。由此 `1.18.11` 也不是当前
Windows 产品候选，并再次证明静态 schema 相容不能代替真实 runtime 验收。

## 2. 生成链

`cargo check -p kaleido-adapter-opencode` 的当前构建报告：

```text
opencode generated subset: 125 schemas
Finished `dev` profile ...
```

审查发现旧规则把 JSON Schema draft-07 本就使用的数值 `exclusiveMinimum` 错写成 draft-04 的
`minimum` + boolean `exclusiveMinimum`。当前链保留原数值约束且无需规范化规则，并移除了不能正确
理解 OpenAPI 3.1 schema 的旧 3.0 解析器。generated REST request/response 与显式
`EventPluginAdded` / `EventSessionNextPromptAdmitted` owner 使 closure 为 125 schemas；规范化产物与
Rust 生成物仍只在构建目录，`schemas/` 原样快照未被回写。

最新 R4/R5 集成候选用隔离安装的精确 Codex `0.147.0` 执行 `cargo xtask schema diff`，exit 0：Codex `0.147.0`、OpenCode
`1.18.16`、ACP schema `1.18.0` 共 288 个 JSON 文件，required-surface 内外均为 0 drift。
这证明当前快照可重复，不推翻真实 runtime 违反同版本 `/doc` 的 D-B11 阻塞。

最新集成候选的完整 `cargo xtask ci` 也 exit 0，依次通过 fmt、check-deps、lint-forbidden、clippy、
Claude sidecar、workspace/doc tests 与 fixtures verify。本页仍不把静态 schema/fixture gate 冒充
OpenCode realtime acceptance。

最新工作树的 OpenCode adapter focused fmt/clippy 已通过，
`cargo test -p kaleido-adapter-opencode --all-targets` 当前 21/21 通过。新增回归锁定：assistant
`parentID` 与 user message 共用 canonical Turn、user/assistant role 不串位、同一 part 更新保持 sequence、
idle 清 active turn、空 discovery 只证明 `HistoryList`、timestamp string 漂移继续变红、generated
REST body/response 与 project scope/abort true、attention 回执的 session/request/reply/answers 精确匹配。
没有扩大 clippy allow，也没有改写真实 fixture 迎合实现。

变异验证实际执行：临时从 reducer 支持表移除 `session.next.prompt.admitted` 后，精确测试
`generated_prompt_admission_timestamp_rejects_the_observed_string_drift` 因
`UnknownEventType` exit 1；恢复该标签后同一测试 exit 0。

SSE reader 已移到 adapter 私有 reader thread，worker 的 `drain_effects` 在无事件时立即返回；
`RespondAttention` 的 HTTP 200 不再完成命令，而是在有限 request timeout 内消费同一 typed SSE
队列，只有精确 matching 的 answered event 且 `AttentionAnswerSource::LocalCommand` 绑定原
`CommandId` 才返回终态 effect。缺回执、拒绝/答案不符或 stream 漂移均返回明确错误，保持命令未完成。

## 3. 真实 fixture

| 文件 | 来源与覆盖 | 不覆盖 |
|---|---|---|
| `tests/fixtures/opencode/01-simple-turn.jsonl` | 真实 OpenCode `1.18.11`；session create、SSE streaming message/part、REST message snapshot | `1.18.16` 新面、permission、abort、强制 reconnect |
| `tests/fixtures/opencode/09-elicitation.jsonl` | 真实 OpenCode `1.18.16`；89 records；question asked/replied、逐题 answer、tool pending/running/completed、reasoning/text、busy→idle 与 REST snapshot | permission allow/deny、abort、SSE transport loss |

`09-elicitation` 的真实 question 选择为 Red，后续结构化 tool output 与 assistant 文本均保留在 fixture。
这不是手写理想 JSONL，也没有被改造成 permission/interrupt 证据。

`01-simple-turn.metadata.json` 记录 cleanup 不完整，仍有一个 descendant PID 无法确认终止；它不影响
JSONL 的协议事实，但不能作为进程树清理门禁。`09-elicitation.metadata.json` 的 cleanup 为 complete。

## 4. 真实 `1.18.16` product live probe

隔离 toy project 中启动真实：

```text
opencode serve --pure --hostname 127.0.0.1 --port 41816
```

较早一次运行仓库产品 adapter 的 `examples/live_probe.rs`，提交
`Reply with the single word READY.`，得到：

```json
{"discovery_effects":7,"start_effects":2,"acceptance_effects":3,"stream_effects":5,"idle":true,"lossless_replay":false}
```

该运行发生在 scoped UACP `0.5.0` 与最终生成型 stream hygiene 落地前，只能作为历史观察，
不能冒充当前端到端 acceptance。

最新 exact probe 使用真实 OpenCode `1.18.16`：

```text
cargo run -p kaleido-adapter-opencode --example live_probe -- \
  http://127.0.0.1:41816 <project-root>
```

进程 exit 1；失败前输出：

```json
{"discovery_effects":11,"start_effects":2,"acceptance_effects":4,"stream_effects":0,"recovery_effects":11,"protocol_recoveries":1,"resume_effects":2,"queue_effects":3,"queue_delivered":true,"idle":false,"realtime_converged":false,"lossless_replay":false}
```

终错定位为 `structured reduction failed at session.next.prompt.admitted`。真实 `/event` 流量与同版本
pinned `/doc` 至少有两处不一致：

- `session.next.prompt.admitted.properties.timestamp` 实际为 string，`EventSessionNextPromptAdmitted`
  要求 number；
- runtime 发送 `/doc` 完全未声明的 `server.heartbeat`。

adapter 坚持使用生成类型并 fail-closed，没有手写 DTO、宽松 `Value` 分支或忽略字段来绕过漂移。
该 probe 证明 REST discovery/start、scoped prompt admission、canonical resume 与 typed queue receipt
可走通；`stream_effects=0`、`idle=false` 与 `realtime_converged=false` 同时证明当前实时投影/恢复门禁
没有通过。因此 D-B11 不能关闭，旧 probe 与 raw fixture 也不能替代当前生成型产品路径验收。

`lossless_replay:false` 是刻意的能力诚实表达：OpenCode `/event` 没有公开 replay cursor，
REST snapshot + 新 SSE tail 不能证明输入无 gap。

## 5. 当前验收矩阵

| 面 | 状态 | 证据/缺口 |
|---|---|---|
| discovery/history | **通过（本地真实 REST）** | `1.18.11` / `1.18.16` REST snapshot；不推出实时通过 |
| streaming turn | **当前产品 gate 被阻塞** | 两份真实 fixture 保留历史/原始协议证据；最新 generated product probe 在首个 typed event 漂移处 fail-closed |
| structured question | **通过（真实 fixture）** | `09-elicitation` |
| prompt admission receipt / queue | **通过（本地真实）** | 最新 probe `acceptance_effects=4`、`queue_delivered=true`；不推出 realtime 或 steer |
| final scoped runtime acceptance | **部分通过** | current probe 可走 prompt/queue scoped receipt；实时 reduction 随后 fail-closed |
| `/doc` ↔ `/event` contract | **阻塞** | timestamp number/string 冲突；未声明 `server.heartbeat`；D-B11 未关闭 |
| permission allow/deny | **未验** | endpoint/reducer 已实现，但本卡没有真实触发 fixture |
| active-turn interrupt | **未验** | abort route/ack 已实现，但没有真实 active-turn receipt |
| forced SSE disconnect/reconnect | **部分** | snapshot-first 非无损恢复、event-id 去重与 per-session live scope 已实现；真实强制中断/gap 场景未齐 |
| unknown/malformed/scope/receipt rejection | **focused 测试通过** | 21/21；仍需最新集成总门禁与真实 permission/abort/reconnect 取证 |
| `cargo xtask schema diff` | **最新集成候选本地通过** | 精确 Codex `0.147.0` 下 exit 0；288 JSON files；仍不能检测 runtime-vs-`/doc` 漂移 |
| `cargo xtask ci` | **最新集成候选本地通过** | 全入口 exit 0；T-111 仍因 D-B11/真实 provider 门禁 active |

## 6. 未完成门禁

- 先取得与真实 `/event` 一致、可生成且可审查的上游合同，或上游修复同版本 `/doc`；禁止手写绕过；
- permission allow/deny、active abort、强制 SSE 断线/重连的真实 provider 证据；
- success/refusal/unknown/reconnect/duplicate/gap 的完整会失败测试与记录的变异红灯；
- T-113 的跨平台 CI 与实体 Android 总门禁。
