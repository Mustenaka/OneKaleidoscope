# R0 文档重新定基线验收

- 日期：2026-07-30
- 结论：**通过**
- 范围：文档与任务状态；未修改产品代码、schema、fixture JSONL 或 proto

## 验收项

- [x] 最终需求保留三家 Agent、CLI/原生 GUI、历史/实时/控制、项目分类、队列与进行中任务。
- [x] 跨 Agent “规划 → 执行 → 审核 → 返工”进入 v1 canonical model。
- [x] Session Broker 区分 `broker_managed`、`shared_runtime`、`external_native`。
- [x] 历史来源与实时运行时分离。
- [x] 原生表面采用六格验收；上游阻塞不算通过。
- [x] 固定 12 事件与 12×3 fixture 门禁退出当前合同。
- [x] 自有 Ubuntu rendezvous/relay 成为 v1 组件；G0 改为性能数据。
- [x] T-001～T-013 冻结，T-014 撤销，新任务从 T-100 开始。
- [x] 旧 KICKOFF 与 M1 执行队列删除。
- [x] Claude Code 主管的下一次交付限定为 PROTOCOL、最小 proto、R1 评审与 T-100。
- [x] 本地 Markdown 相对链接检查无断链。
- [x] `git diff --check` 通过。

## 仓库自检

| 命令 | 结果 |
|---|---|
| `cargo fmt --all -- --check` | 通过 |
| `cargo clippy --all-targets -- -D warnings` | 通过 |
| `cargo test --workspace` | **163 passed / 1 failed** |

失败项是冻结 recorder 的既有测试
`agents::codex::tests::outside_permission_target_is_rejected_with_redacted_structured_diagnostics`：
临时目录位于用户目录下时诊断先脱敏为 `<HOME>`，旧断言要求 `<OUTSIDE_PATH>`。
本次只做文档重新定基线，不修改或放宽该测试；它不阻塞 R0，也不得重新升级为产品开工门禁。

## 保留资产

- `schemas/`：上游快照与漂移证据；
- `tests/fixtures/`：真实协议录制；
- `spikes/`、`xtask/`：研究工具与工程守卫；
- ADR-0001～0008：历史决策和实测事实。

这些资产按需引用，不再阻塞产品纵切。

## R0 之后仍未完成

R0 通过不代表可以立即写 adapter。项目主管必须完成 R1：

1. `docs/PROTOCOL.md`；
2. `crates/kaleido-proto`；
3. 最小 Swift/Kotlin UniFFI 验证；
4. T-100 单 Provider 本地纵切任务卡。

Codex/Claude 原生 GUI 的第三方实时附着仍是公开接口阻塞；OpenCode GUI 是否共享 server 仍需真实验收。
