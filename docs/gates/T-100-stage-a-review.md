# T-100 阶段 A 评审

- 日期：2026-07-31
- 实现方：Claude Code（Implementer）
- 评审：项目主管（Claude Code，Orchestrator）
- 结论：**通过。放行阶段 B。**

阶段 A 是离线 replay 纵切：`Codex 录制 → 钉定路径 decoder → reducer → canonical state
→ durable log → 六个投影`。四个新 crate 共约 5000 行实现 + 1700 行测试。

---

## 1. 主管独立复验（不是照抄交付说明）

### 1.1 复跑

```text
cargo xtask ci
<== fmt-check: ok
<== check-deps: ok; 9 workspace member(s), 9 internal edge(s), 6 crates/* manifest(s)
<== lint-forbidden: ok
<== clippy: ok
<== test: ok
<== fixtures-verify: ok; 5 file(s), 220 record(s)
exit 0

cargo test -p kaleido-proto → 36 passed（与 R1 基线逐字一致）
```

### 1.2 合同未被擅改：给出 git 之外的证据

实现方指出 `crates/` 整体未提交，git 无法直接为 `kaleido-proto` 出具「未改」证明。
主管用文件修改时间补上了这条证据：

```text
kaleido-proto/src/*.rs        07-30 18:51 ～ 19:25   （R1 时段）
kaleido-proto/src/turn.rs     07-30 21:50            （主管 R1 评审时自己的变异，已还原）
kaleido-proto/src/attention.rs 07-30 21:50           （同上）
kaleido-proto/tests/contract.rs 07-30 19:25          （R1 时段）
新四个 crate                   07-30 22:45 ～ 23:13
```

proto 的最后写入时间早于四个新 crate 的第一次写入，且唯二的例外正是主管自己在 R1
评审时做的两处变异。合同未被本卡触碰。

`git diff Cargo.toml` 只有 R1 已有的两行（`members` 加 `crates/*`、`uniffi` 版本），
本卡零改动。`git diff Cargo.lock | grep "^+name"` 中**没有** `sha2` 与 `tempfile`，
证明新增依赖复用了已在锁文件里的精确版本，第三方新增包为 0。

### 1.3 主管自己的变异验证（刻意选实现方**没有报告**的位置）

实现方自报了六处。为防止测试对自报变异过拟合，主管另选三处下手：

| # | 改坏内容（实现方未报告） | 结果 |
|---|---|---|
| A | `ContentStore::store` 里把正文 `String::from_utf8_lossy(bytes)` 打进 `tracing::debug!` | **变红**：``` `KALEIDO SIMPLE TURN` appears in tracing output ``` |
| B | `Store::submit_command` 的幂等查表改成恒不命中 | **变红**（2 条）：`a_repeated_command_is_reported_as_duplicate_and_appends_nothing`、`idempotency_survives_a_reload` |
| C | `Store::apply` 的 trace 里加 `debug_effect = format!("{effect:?}")`（整个 `StateEffect` 的 Debug） | **未变红** —— 见下 |

C 未变红**不是测试漏洞**，而是一条值得记录的架构性质：canonical `StateEffect` 内部
根本不携带正文、完整路径或上游原始 ID（它们都在 `ContentRef` 后面），所以把整个 effect
Debug 出来也不构成 §10 泄漏。脱敏测试是**内容**测试而不是形状测试，这是正确的设计——
A 证明了只要真的有正文流出，它立刻会红。

三处改坏全部还原，`cargo xtask ci` 重新 exit 0。

### 1.4 六个投影的实际输出抽查

主管自己跑了 `slice replay --fixture 04-permission-deny.jsonl` 再 `slice show`：

```text
TURN status=completed error=None items=6
   seq=0 user_message   completed
   seq=1 reasoning      completed
   seq=2 agent_message  completed
   seq=3 file_edit      declined      ← R-P8：拒绝是 Item 终态
   seq=4 reasoning      completed
   seq=5 agent_message  completed     ← 6 条，不是 turn/completed summary 里的 1 条
```

`RuntimeCapabilityView` 20 项：

```text
turn_prompt              supported      recorded_fixture
interaction_approval     supported      recorded_fixture
state_tool_lifecycle     supported      recorded_fixture
turn_steer               not_verified   absent          ← R-P9：没有证据就不宣称
live_observe             not_verified   absent          ← R-P7：replay 不冒充实时
其余 15 项                not_verified   absent
```

`turn_prompt = supported / recorded_fixture` 经核对 §4.2 合法：协议只对
`LiveBinding::Observing` 与 `DeliveredAsSteer` 强制要求 `ObservedInTraffic`，
能力项允许 `RecordedFixture` 且证据来源对 UI 可见。

durable log 文件扫描：真实上游 thread id、`KALEIDO*` 正文、`editable.txt` 均**未出现**。

### 1.5 `CLAUDE.md` §6 逐条

| 检查项 | 结论 |
|---|---|
| 擅改 proto / 协议语义 | 无（§1.2） |
| PTY / TUI / ANSI / 屏幕 / transcript 轮询冒充实时 | 无。四个 crate 全文无相关依赖或解析代码 |
| 按 provider 名称硬编码能力 | 无。`kaleido-state` / `kaleido-adapter` / `kaleido-hostd` 全文无 `"codex"` 等字面量分支；A-2 豁免 0 |
| queue 冒充 steer / history 冒充 live / decline 冒充 error | 三处均正确，且各有变异证据（实现方 ①②、主管 §1.4 实测） |
| 测试改坏是否真变红 | 是，含主管另选的两处（§1.3） |
| 日志 / 推送泄漏 | 无（§1.4），且泄漏发生时测试会红（§1.3 A） |
| 扩大到无关范围 | 无。新文件全部在四个新 crate 目录下 |

---

## 2. 偏离裁定

| # | 实现方偏离 | 裁定 |
|---|---|---|
| ① | 只建模 file-change 一族审批 | **接受，且实现方是对的。** 主管对着 `schemas/codex/*.json` 复核：`PermissionsRequestApprovalResponse` 的 `required` 是 `permissions`，根本没有 `decision`；`CommandExecutionRequestApprovalResponse` 的 `decision` 是与 file-change 不同构的 `oneOf`。原 §11.1 的「同上」是主管未核对回复合同的纸面推断。已由 [ADR-0014](../adr/0014-codex-approval-families-and-timestamp-units.md) 修正协议与任务卡 |
| ② | `commandExecution` item 未解码 | **接受。** 无一手录制，注册它会在「无死条目」检查里变成死条目。走 `UnknownUpstreamLabel` 是正确处理 |
| ③ | §5.4 的阶段划分自相矛盾 | **主管的错误，已修。** 原文「A = 后两条、B = 第三条」漏掉第一、二条并重复分配第三条。实现方采取的读法（A = 1/2/4，B = 3）是唯一可执行的读法，已写进任务卡 §5.0 |
| ④ | 加 `--projection all` 与 `--base-at-ms`；`slice run` 不留桩 | **接受。** 追加取值不破坏既有契约；不留桩是对的——存在但不能用的子命令比不存在更糟 |
| ⑤ | 新增 `sha2 =0.11.0`、`tempfile =3.27.0`（dev-only） | **接受。** 已核对 `Cargo.lock` 无新第三方条目，均为 workspace 内已用的精确版本 |
| ⑥ | replay 中 `LiveBinding` 恒为 `NotBound` | **接受，这正是要的。** 录制证明协议形状，不证明「现在附着着」。主管已实测确认 `live_observe = not_verified` |

---

## 3. 新登记的协议缺口

| ID | 内容 | 处置 |
|---|---|---|
| **P-1** | `AttentionState::Answered` 强制 `command_id`，但 replay 的决定来自被录制的客户端，本地无对应 `CommandEnvelope`。当前铸了确定性 ID 代表「被观察到的决定」 | 阶段 A 接受为权宜之计（代码注释已写明）。**R3 前必须改协议**，让 `Answered` 能区分本地应答与观察到的外部应答。改 proto 需单独开卡授权，不得夹带进 T-100。见 [ADR-0014](../adr/0014-codex-approval-families-and-timestamp-units.md) D-3 |
| **P-2** | Codex 审批不带过期时间，`ApprovalExpired` 在真实流量里永不触发 | 上游事实，非缺陷。移动端必须能渲染「无过期时间」的审批，不得假定都有倒计时 |
| — | `tracing` 的 callsite interest 是进程级缓存，脱敏扫描必须独占 test binary | 工程发现，处理正确（`tests/tracing_redaction.rs` 独立 + 空捕获断言）。已写进 [DEVELOPMENT.md](../DEVELOPMENT.md) 防止未来被合并回去 |

---

## 4. 判定

**阶段 A 通过。放行阶段 B。**

阶段 A 的 DoD 全部取得机器证据，没有出现「因环境原因未取得证据」的格子——这是阶段
划分要达到的效果，达到了。

阶段 B 的范围**不变**：`slice run` 接真实 `codex app-server`，补 §5.1 第四条、§5.3、
§5.4 第三条与 §6 真实验收。三条提醒：

1. 阶段 A 的断言一条都不许放宽。真实 Codex 跑不通就按 §6.4 登记阻塞；
2. `turn_steer` 在真实运行下大概率仍然拿不到 `ObservedInTraffic` 证据——那就让它
   保持 `not_verified`，这是正确结果，不是待修的缺陷；
3. P-1 不在阶段 B 范围内，不要顺手改 proto。
