# T-103 验收证据：Attention 应答来源

> 日期：2026-08-09
> 分支：`codex/t-103-attention-answer-provenance`
> 基线：`main@75f3588`

## 结论

P-1 已按 [ADR-0018](../adr/0018-attention-answer-provenance.md) 解决：

- UACP 升级到 `0.2.0`；旧 `0.1.x` 不再属于兼容范围；
- `AttentionState::Answered` 使用闭合的 `AttentionAnswerSource`；
- 本地回答保存真实 `CommandEnvelope.command_id`；
- 无法关联到本地命令的实时回答与真实 fixture 回放分别保存
  `ObservedInTraffic` / `RecordedFixture` 证据；
- 不再为外部回答铸造 `CommandId`；
- 旧 `0.1` Answered 日志不做启发式迁移，加载时 fail-loud 为
  `StateError::MalformedRecord`。

## 本地测试

恢复态的定向测试结果：

```text
kaleido-adapter-codex reduce_fixtures: 20 passed; 0 failed
kaleido-proto contract:                38 passed; 0 failed
kaleido-state store:                    9 passed; 0 failed
kaleido-hostd slice:                   10 passed; 0 failed
kaleido-adapter neutral session:        4 passed; 0 failed
kaleido-adapter-codex runtime_session:  4 passed; 0 failed
```

`cargo check -p kaleido-proto --features uniffi` 通过。完整门禁：

```text
cargo xtask ci: exit 0
fmt-check / check-deps / lint-forbidden / clippy / workspace test: ok
fixtures-verify: 5 files, 220 records
```

Windows 上还以新类型图成功生成 Kotlin 与 Swift UniFFI 源码；外语消费端编译由
Ubuntu/macOS Actions 门禁给出最终证据。

三平台 Actions 结果在最终提交后补记。

## 真实 Codex 0.147.0 纵切

使用真实 `codex app-server` 和临时项目执行文件修改请求，Broker 接受首个审批：

```text
codex-cli 0.147.0
slice run exit: 0
termination: turn_terminal
file content after approval: after T-103 approval
answer_source.kind: local_command
attention option: accept
matching CommandAcknowledged records: 1
matching command outcome: accepted_locally
```

持久状态中的 `LocalCommand.command_id` 与日志里的真实
`CommandAcknowledged.command_id` 完全相同。临时路径、上游请求 ID、prompt 正文与文件路径
均未写入本证据文档。

## 错误路径与变异

三处独立故障注入均在恢复实现前验证为红：

1. 把未关联 fixture 回答伪装成 `LocalCommand`：
   `an_approved_file_change_completes_and_its_approval_is_answered` 失败；
2. 删除 `observer_host_id == AttentionItem.host_id` 校验：
   `attention_answer_source_rejects_empty_or_cross_host_evidence_and_old_wire_shape` 失败；
3. 为缺失 `answer_source` 的旧日志添加猜测默认值：
   `a_zero_one_answered_log_fails_loud_without_migration` 失败。

此外，测试覆盖空本地命令 ID、空 observer host、外部已回答后新本地命令返回
`ApprovalAlreadyAnswered`、相同幂等键仍返回 `Duplicate`，以及本地 wire 回显不会覆盖
Store 已保存的真实来源。

## 边界与未宣称内容

- 未修改 `schemas/**`、真实 fixture、workflow、transport 或 T-104 的控制语义；
- 版本兼容谓词已拒绝 `0.1.x`，但当前仓库尚无调用该谓词的 live transport handshake，
  因此本卡不声称已在网络握手中实测拒绝旧 peer；
- 旧 store 只能归档并从可信的 runtime/fixture 证据重建，本卡没有 migrator。
