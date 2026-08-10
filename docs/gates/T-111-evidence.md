# T-111 OpenCode / D-B11 evidence

> 状态：**active evidence ledger，不是完成证明**
> 基线：`origin/main@893e930`
> 记录日期：2026-08-10

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
unverified warning。`npm view opencode-ai version` 与 `npm view @opencode-ai/sdk version` 均返回
`1.18.16`，因此下述漂移不能解释为 CLI/server 与 SDK 的公开版本号不一致。

## 2. 生成链

`cargo check -p kaleido-adapter-opencode` 的当前构建报告：

```text
opencode normalization numeric_exclusive_minimum_to_bound hit 25
opencode generated subset: 117 schemas
Finished `dev` profile ...
```

当前只保留这一条实际命中规则；真实 `1.18.16` 快照测试锁定命中数 25，单元测试锁定
before/after 等价形状。新增生成的 `EventPluginAdded` / `EventSessionNextPromptAdmitted` 使实时流卫生
检查也走 `/doc` 生成类型，当前 closure 为 117 schemas。规范化产物与 Rust 生成物只在构建目录，
`schemas/` 快照未被生成器回写。

最终本地候选已执行 `cargo xtask schema diff`，exit 0：Codex `0.147.0`、OpenCode
`1.18.16`、ACP schema `1.18.0` 共 288 个 JSON 文件，required-surface 内外均为 0 drift。
这证明当前快照可重复，不推翻真实 runtime 违反同版本 `/doc` 的 D-B11 阻塞。

第三次完整 `cargo xtask ci` 也 exit 0，依次通过 fmt、check-deps、lint-forbidden、clippy、
Claude sidecar、workspace tests 与 fixtures verify。本页仍不把静态 schema/fixture gate 冒充
OpenCode realtime acceptance。

OpenCode adapter focused fmt/clippy 已通过，`cargo test -p kaleido-adapter-opencode --all-targets`
当前 11/11 通过。测试 lint 使用 `get`/`pointer`/`first` 与显式错误传播，没有扩大 clippy allow，
也没有改写真实 fixture 迎合实现。

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
| unknown/malformed/scope rejection | **focused 测试通过** | 仍需总门禁与要求中的完整错误矩阵 |
| `cargo xtask schema diff` | **本地通过** | exit 0；288 JSON files；required-surface 内外 0 drift；不能检测本次 runtime-vs-`/doc` 漂移 |
| `cargo xtask ci` | **本地通过** | 第三次完整运行 exit 0；T-111 仍因 D-B11/真实 provider 门禁 active |

## 6. 未完成门禁

- 先取得与真实 `/event` 一致、可生成且可审查的上游合同，或上游修复同版本 `/doc`；禁止手写绕过；
- permission allow/deny、active abort、强制 SSE 断线/重连的真实 provider 证据；
- 把 blocking SSE reader 与 command dispatch 解耦，避免空闲流量把移动命令延迟到 HTTP timeout；
- success/refusal/unknown/reconnect/duplicate/gap 的完整会失败测试与记录的变异红灯；
- T-113 的跨平台 CI 与实体 Android 总门禁。
