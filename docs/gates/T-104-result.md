# T-104 验收结果

- 日期：2026-08-09
- 基线：`main@75f3588`
- 分支：`codex/t-104-live-control-evidence`
- 实现提交：`bc943c1`
- Pull Request：[PR #3](https://github.com/Mustenaka/OneKaleidoscope/pull/3)
- 结论：**通过。`LiveControl` 与 `LiveBinding::Controlling` 已有真实、可审计且 fail-closed 的可达路径。**

---

## 1. 合同结论

[ADR-0019](../adr/0019-live-control-runtime-acceptance.md) 已由用户以项目主管身份批准，且在实现提交之前独立落库。

采用语义方案 A：`LiveControl` 是 Session/live connection 级的聚合事实——本 Broker 的至少一条
改变 Session 状态的 canonical 命令，已经被 runtime 明确接受。它不替代具体动作能力，也不推出
`TurnSteer`、`InteractionApproval` 或其他能力。

唯一合格证据是与真实 `CommandEnvelope.command_id` 相关联的
`CommandOutcome::AcceptedByRuntime`。`AcceptedLocally`、发送成功、replay、版本判断和无关联流量都
不能证明控制。

## 2. 实现与状态不变量

Codex `SubmitPrompt` 纵切现在执行以下有序事实链：

```text
AcceptedLocally
  → TurnOrigin::RemoteCommand { 同一 command_id }
  → AcceptedByRuntime { RuntimeAcknowledgement }
  → LiveControl::Supported
  → LiveBinding::Controlling
```

单帧内依赖顺序固定为：

```text
TurnUpserted → CommandAcknowledged → CapabilitiesUpdated → SessionUpserted(Controlling)
```

canonical write path 额外拒绝：伪造的 public `AcceptedLocally`、无前序/重复/跨 runtime 的 runtime
ack、跨 Session 借用证据、一个命令关联多个 Turn、既有 Turn 身份被改写，以及无法由候选 Session
自身解析 runtime 的 live binding 更新。拒绝发生在 durable append 之前。

## 3. 真实 Codex 纵切

使用 `codex-cli 0.146.0` 和最终实现提交执行真实 `slice run`。以下是从实际 machine report 与
durable log 抽取的合同字段；未记录 prompt、完整路径、canonical/raw ID 或正文：

```json
{
  "slice_exit": 0,
  "termination": "turn_terminal",
  "controlling_kind": "controlling",
  "controlling_evidence_source": "observed_in_traffic",
  "local_ack_count": 1,
  "runtime_ack_count": 1,
  "same_command_id": true,
  "remote_turn_uses_same_command_id": true,
  "runtime_handle_kind": "runtime_acknowledgement",
  "live_control_state": "supported",
  "live_control_evidence": "observed_in_traffic",
  "turn_steer_state": "not_verified",
  "turn_steer_evidence": "absent",
  "steer_supported": false,
  "final_live_binding_kind": "not_bound",
  "reload_final_binding_kind": "not_bound"
}
```

`SessionIndexView` 在 runtime 接受 prompt 后锁存 `controlling / observed_in_traffic`；
`RuntimeCapabilityView` 同时显示 `live_control = supported / observed_in_traffic`。进程正常结束后最终
投影及 durable reload 均回到 `not_bound`，证明历史控制证据不会伪装成当前在线。`turn_steer` 与
`InputQueueView.steer_supported` 始终未被顺带提升。

## 4. 正负路径与真实变异

自动化覆盖以下负路径：

- fixture replay、无本地 correlation、仅发送 request、错 request ID、JSON-RPC error、response
  前进程退出：均不产生 runtime ack、`LiveControl` 或 `Controlling`；
- 只有 `LiveObserve` 时，canonical store 拒绝 `Controlling`；
- `TurnSteer` 保持 `NotVerified / Absent`，排队 steer 保持 `Pending`；
- runtime ack 的 runtime、handle kind、Session、Turn 与 command correlation 任一不一致即拒绝；
- 状态拒绝后日志不追加，reload 保持一致。

本轮实际执行并恢复了超过三处“改坏 → 变红”，包括：

1. 把 `AcceptedLocally` 当 runtime 证据，能力测试变红；
2. 允许 `RecordedFixture` 注册本地 correlation，replay 测试变红；
3. 把 `CapabilitiesUpdated` 放到 runtime ack 之前，store-safe ordering 测试变红；
4. 在 hostd response 前取消 correlation，真实纵切组合测试变红；
5. 分别移除 command/Turn 唯一性、runtime 一致性、同 Session 控制证据、候选 runtime 引用和
   Turn 身份稳定性守卫，对应 state 测试均变红。

所有变异均已恢复，最终工作树不处于变异态。

## 5. 门禁结果

本地最终运行：

```text
cargo xtask ci
exit 0

kaleido-adapter:       unit 6 passed; integration 4 passed
kaleido-adapter-codex: unit 2; fixture 23; runtime 4; surface 6 passed
kaleido-hostd:         slice 11; tracing 1 passed
kaleido-state:         unit 7; store 18 passed
kaleido-proto:         contract 36 passed
real fixtures:         5 fixtures / 220 records passed
clippy:                -D warnings passed
```

实现提交 `bc943c1` 的三平台 CI 全绿：
[run 31307860641](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/31307860641)。Ubuntu
同时编译 Kotlin UniFFI consumer probe，macOS 编译 Swift probe，Windows 执行仓库门禁。

## 6. 边界与偏离

- 未修改 `crates/kaleido-proto/**`，没有新的 wire shape 或协议版本提升；
- 未修改 `schemas/**`、`tests/fixtures/**`、`spikes/**`；
- 未新增依赖；
- 未扩展到 queue/steer 状态机、transport、移动端、其他 provider 或 workflow；
- 未使用 PTY/TUI 抓屏；真实证据全部来自 Codex app-server 结构化 JSON-RPC；
- 与任务卡无偏离，未发现需要另开 ADR 的协议缺陷。
