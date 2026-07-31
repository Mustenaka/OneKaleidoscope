# T-100 门禁结果（两阶段合并判定）

- 日期：2026-07-31
- 实现方：阶段 A = Claude Code；阶段 B = Codex
- 评审：项目主管（Claude Code, Orchestrator）
- 结论：**通过。R2 完成。**
- 阶段 A 单独评审见 [T-100-stage-a-review.md](T-100-stage-a-review.md)

T-100 是重新定基线后的第一个端到端纵切：
`真实 Codex app-server → 钉定路径 decoder → reducer → canonical state → durable log → 六个投影`，
离线可重放、重启可重建、真实进程可观察可干预。

---

## 1. 主管独立复验

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
```

### 1.2 合同未被触碰

实现方提供了 371 个受保护文件的前后聚合哈希（`MATCHES_INITIAL=True`）。主管另用文件
修改时间独立复核：

```text
kaleido-proto/src/*.rs、tests/contract.rs   07-30 18:51 ～ 19:25   （R1 时段）
kaleido-proto/{turn,attention}.rs           07-30 21:50            （主管 R1 评审时自己的变异，已还原）
阶段 A 产物                                  07-30 22:36 ～ 23:23
阶段 B 产物                                  07-31 11:36 ～ 11:53
```

两条独立证据一致。此外 `surface.rs` 停留在 07-30 23:23 —— 阶段 B **没有新增钉定路径**，
实时路径完全复用阶段 A 已验证的解码表，这正是这张卡要证明的架构性质。

### 1.3 主管自己的变异验证（刻意选实现方**未报告**的位置）

实现方自报一处（`ConnectionState::Unavailable { ProcessExited }` → `Disconnected`）。
主管另选两处：

| # | 改坏内容 | 结果 |
|---|---|---|
| D | 去掉 live 模式对合成 `AttentionUpserted` 的抑制，让 reducer 用铸造的 command ID 覆盖 store 的真实应答 | **变红**：`a_live_outgoing_reply_never_overwrites_the_stores_real_answer` |
| E | runtime 退出时不再把 `live_binding` 置为 `NotBound { RuntimeExited }` | **变红**（2 条）：`an_early_process_exit_emits_the_runtime_session_and_fault_triad`、`a_clean_disconnect_has_no_connection_fault_attention` |

D 这处特别值得记：实现方把它写在「偏离说明」里而**没有**给变异证据，是最容易蒙混过去的
位置。它不但有测试，还有一条配对测试
`a_live_outgoing_reply_still_validates_the_recorded_decision_vocabulary`
证明「抑制」没有顺带跳过校验——抑制的是重复 effect，不是校验。

两处均已还原，`cargo xtask ci` 重新 exit 0。

### 1.4 真实运行证据的独立核对

主管没有采信摘要，直接从实现方留下的 durable log 重建投影：

```text
# 运行 3（decline）——由 durable log 重建，即重启路径本身
turn status=completed error=None items=6
   seq=3 file_edit  declined       ← R-P8：拒绝是 Item 终态，Turn 仍 completed
session[history] status=offline live=not_bound queue=0 attention=0

# 运行 2（accept + steer 入队）
QUEUE steer_supported=False writable=False
  entry intent=steer_active_turn state=pending      ← 从未 delivered_as_steer
live_observe           supported      observed_in_traffic   ← 真实附着，与 replay 不同
turn_prompt            supported      observed_in_traffic
interaction_approval   supported      observed_in_traffic
turn_steer             not_verified   absent                ← 没有证据就不宣称
turn_interrupt         not_verified   absent
```

**这正是阶段 B 要证明的那一对差别**：同一套 reducer，证据源从 `RecordedFixture` 换成
`ObservedInTraffic` 后，`LiveBinding` 从 `NotBound` 变成 `Observing`，而 `turn_steer`
**仍然**是 `not_verified`。没有一处被顺手提升。

### 1.5 脱敏独立核对

主管把 durable log 本体与内容寻址存储分开扫描：

```text
target/t100-log/*/streams/**   仅命中 "kaleido-host" / "kaleido-slice"（我们自己的显示名，非泄漏）
                               正文、steer 文本、完整路径、上游原始 ID、审批 method 均未出现
target/t100-log/*/content/**   正文在此，例如 3b3060aa… 的内容确为 "please also mention DONE"
                               日志里只有该 content_id 的引用
```

引用与正文的分离在真实运行数据上成立，不只是在测试里。

### 1.6 `CLAUDE.md` §6 逐条

| 检查项 | 结论 |
|---|---|
| 擅改 proto / 协议语义 | 无（§1.2，两条独立证据） |
| PTY / TUI / ANSI / 屏幕 / 轮询冒充实时 | 无。传输层是 stdio JSON-RPC，报文经 `TranscriptFrame::from_wire` 进同一 reducer |
| 按 provider 名称硬编码能力 | 无。A-2 豁免 0；能力全部来自 `CapabilityProbe` 的观察结果 |
| queue 冒充 steer / history 冒充 live / decline 冒充 error | 三处均正确，且在**真实运行数据**上验证（§1.4） |
| 测试改坏是否真变红 | 是，含主管另选的两处（§1.3） |
| 日志 / 推送泄漏 | 无（§1.5） |
| 扩大到无关范围 | 无。阶段 B 只动 adapter-codex、hostd 与各自测试 |

---

## 2. DoD 判定

| 格 | 阶段 | 结论 |
|---|---|---|
| §5.1 成功路径（前三条 replay + 第四条真实流式） | A + B | 通过。真实运行观察到非空 `streaming_item_ids` |
| §5.2 审批与拒绝（R-P8，含 join 乱序、未知、scope mismatch、重复/过期回复） | A | 通过 |
| §5.3 队列与能力诚实（R-P9、R-P6） | B | 通过，真实数据见 §1.4 |
| §5.4 错误路径（未登记 method / pointer 失败 / 进程退出 / 日志跳号） | A + B | 通过 |
| §5.5 重启路径（R-P4、§5.4 收敛口径） | A + B | 通过，结构相等 |
| §5.6 漂移守卫（ADR-0012 D-2） | A | 通过，41 条钉定路径 |
| §5.7 安全（§10） | A + B | 通过，见 §1.5 |
| §5.8 工程门禁 | A + B | 通过，exit 0 |
| §5.9 测试真实性 | A + B | 通过，实现方 7 处 + 主管 5 处 |
| §6 真实验收 | B | 通过。Codex `0.146.0`，三次运行 + 一次重启重建 |

**R2 门禁**（[MILESTONES](../MILESTONES.md)）：项目与会话列表、一次流式 turn、
工具生命周期与 approve/deny、状态按真实能力呈现、queue/steer/interrupt 的能力差异诚实
可见、进程重启后恢复、未知消息与 join 失败有错误路径——**全部达成**。

---

## 3. 阶段 B 偏离裁定

| # | 偏离 | 裁定 |
|---|---|---|
| 1 | 运行 3 前把 `editable.txt` 恢复为 `ORIGINAL`，否则 Codex 判断「无需修改」而不再发审批 | **接受，实现方是对的。** 任务卡 §6.2 漏了这一步，已补 |
| 2 | 运行 4 用 `--projection all` 而非卡里写的 `transcript` | **接受。** 卡正文与 §6.3 要求的六个投影证据自相矛盾，已改为 `all` |
| 3 | 首次运行 1 被外层工具 10 秒等待窗口中断，隔离到 `target/t100-log/a-interrupted` 后重跑 | **接受。** 处理正确：隔离而非覆盖，并明确声明不作为验收证据 |
| 4 | `respond_attention` 不携带本地 `command_id`；live 模式抑制 reducer 的合成 `Answered` effect | **接受，且这是正确做法。** store 已用真实 command ID 应答，reducer 的铸造 ID 若覆盖上去就是把真实状态换成假的。主管已用变异 D 确认有测试守住 |
| 5 | 跨 stream 投影的 cursor 语义（session 更新影响 `SessionIndexView`，但 envelope 的 project cursor 仍为 `seq = 0`） | **登记为 D-B3，不阻塞。** 实现方没有擅改阶段 A 的 cursor 合同，处理正确 |

---

## 4. 主管新发现的问题（实现方未报告）

### D-B1：`LiveControl` 结构性不可达，`LiveBinding::Controlling` 永远到不了

`Capability::LiveControl` 在 `crates/kaleido-adapter/src/capability.rs` 里只出现在
`ALL_CAPABILITIES` 枚举列表中，**没有任何代码路径会把它标记为 proven**。

但真实运行里，broker 确实通过这条 live 连接提交了 prompt、并回答了审批，runtime 也都
接受了——按 [PROTOCOL](../PROTOCOL.md) §4.3，`Controlling` 要求 `LiveObserve` +
`LiveControl`，这两件事本身就是控制证据。

**这不构成阶段 B 不通过**：方向是「少说」而不是「多说」，协议规定未观察即
`NotVerified`，T-100 的 DoD 也从未要求 `Controlling`。

**但它是 R3 的真问题**：手机需要靠 `LiveBinding::Controlling` 判断「我现在能不能干预」。
如果永远到不了，手机会一直渲染成只读。**R3 开工前必须处理**：要么让命令被 runtime 接受
时提升 `LiveControl`，要么在协议里说明 `LiveControl` 的确切含义与 `TurnPrompt` 有何不同。

### D-B2：进程树终止只有「已退出」这一条测试

`crates/kaleido-adapter-codex/src/platform/windows.rs` 的 `terminate_tree` 实现是稳妥的
（`taskkill /PID /T /F`，多级回退，taskkill 自身也带 `CREATE_NO_WINDOW`），但唯一的测试
`an_already_exited_child_is_a_successful_tree_termination` 只覆盖**子进程已经退出**的分支。
真正的「杀掉一棵活着的进程树、不留孤儿」没有测试。

任务卡 §3.3 明确要求「退出时杀整个进程树」。真实验收运行确实走过这条路径，但没有断言。
**R4 前必须补**：那时 hostd 变成长期驻留的服务，孤儿 runtime 才会真正开始积累。

> 主管无法独立复核「退出后 codex 进程数为 0」：本次实现方本身就是 Codex，
> `codex.exe` 在评审时仍在运行属正常。这不是矛盾，是无法falsify——所以更需要 D-B2 的测试。

---

## 5. 携带项总表

| ID | 内容 | 阻塞谁 |
|---|---|---|
| UB-R1-S | Swift UniFFI 绑定编译 | R8 |
| G-R1-1 | UniFFI 的 callback / object / async / throwing 面未探针 | R3 |
| P-1 | `AttentionState::Answered` 无法表达「观察到的外部应答」 | R3 |
| **D-B1** | `LiveControl` 不可达，`Controlling` 永远到不了 | **R3** |
| **D-B2** | 活进程树终止无测试 | R4 |
| D-B3 | 跨 stream 投影的 cursor 语义待确认 | 待定，不阻塞 |
| P-2 | Codex 审批无过期时间 | 不阻塞，移动端需正确渲染 |
| D-R1-1 | `xtask check-deps` 的 TOML 读取器不支持点号键 | 不阻塞 |

任何时候都不许把上述任何一条在文档里写成已通过。

---

## 6. 判定与下一步

**T-100 通过，R2 完成。**

下一步按 [MILESTONES](../MILESTONES.md)：

1. **下发 [T-102](../tasks/T-102.md)** —— 解除 UB-R1-S（macOS CI 上真实 `swiftc` 编译）
   并探明 UniFFI 的 callback / object / async / throwing 面（G-R1-1）；
2. T-102 通过后，R3 开工前还要结清 **P-1** 与 **D-B1**，两者都要改 `kaleido-proto` 或
   协议，必须单独开卡授权；
3. 然后才是 R3（Android 局域网纵切）。

不要在 T-102 之外自行扩大范围。
