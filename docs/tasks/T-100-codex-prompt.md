# T-100 下发给 Codex 的 prompt（原样粘贴）

> **状态：active，2026-07-30 下发。**
> R1 已按 [ADR-0013](../adr/0013-platform-track-order.md) 有条件通过：Kotlin 绑定编译通过，
> Swift 编译作为 UB-R1-S 携带至 R8（见 [T-102](T-102.md)），不阻塞本卡。
>
> 这是重新定基线后的第一张产品任务卡。T-001～T-013 已冻结，T-014 已撤销；旧 prompt 一律不得复用。
> **本卡分两阶段交付，阶段 A 完成后必须停下来等评审。**

---

你是 OneKaleidoscope 项目的**实现方（Implementer）**。架构与协议由项目主管定义，你负责把任务卡变成可运行、可验证的代码。你的交付会被逐条审核。

## 动手前必读（按顺序，全文，不要跳读）

1. `AGENTS.md` —— 你的行为规范。尤其 §2 铁律、§3 编码规范、§4 交付格式、§5 阻塞报告格式
2. `docs/tasks/T-100.md` —— **本次任务卡。里面的 Definition of Done 是唯一验收标准**
3. `docs/PROTOCOL.md` —— 全文。这是本项目第一次有协议合同，你的所有类型都从它推导
4. `crates/kaleido-proto/src/**` —— 合同的代码形态。看完再动手
5. `docs/adr/0012-provider-decode-strategy.md` —— 决定了你**不生成**上游类型，而是钉定路径解码
6. `docs/adr/0009-session-broker.md`、`docs/adr/0010-canonical-state-and-workflow.md`
7. `docs/adr/0013-platform-track-order.md` —— 为什么 R1 可以带着 Swift 阻塞往前走，以及这**不是**降低标准的先例
8. `tests/fixtures/codex/01-simple-turn.jsonl`、`03-permission-approve.jsonl`、`04-permission-deny.jsonl` —— 真实录制的 Codex 报文，你的映射依据

`docs/STATUS.md` 现在应显示 R1 有条件通过、T-100 active。如果不是，停下来问主管。

## 本次任务

执行 `docs/tasks/T-100.md`：把一个真实 Codex app-server 会话，经
decoder → reducer → canonical state → durable log → 投影，做成可观察、可干预、
可重启恢复的本地纵切。新建四个 crate：`kaleido-state`、`kaleido-adapter`、
`kaleido-adapter-codex`、`kaleido-hostd`。

## 最重要的一条：分两阶段，中间停一次

上一轮 M1 是怎么死的：一张大卡闷头做很久，交付时方向已经偏了。这次不许再这样。

**阶段 A（离线，必须先做完并单独交付）**

1. `kaleido-state`：apply / 日志 / load / 快照 / 六个投影 / 幂等；
2. `kaleido-adapter` 的 provider 中立 trait；
3. `kaleido-adapter-codex` 的 `surface.rs` + `decode.rs` + `reduce.rs`，用三份 fixture 驱动；
4. `kaleido-hostd` 的 `slice replay` 与 `slice show`。

打绿 T-100 §5.1 前三条、§5.2 全部、§5.4 后两条、§5.5、§5.6、§5.7、§5.8、§5.9。

阶段 A 的每一条 DoD 都只依赖已提交的 fixture，**不依赖登录、网络或本机 Codex 版本**。
所以阶段 A **不接受任何「因环境原因未取得证据」**。做完写一份「T-100 阶段 A 交付」报告，
**停下来等主管评审**。不要顺手就往下做。

**阶段 B（真实进程，评审通过后再做）**

`slice run` 接真实 `codex app-server`，补 §5.1 第四条、§5.3、§5.4 第三条和 §6 的真实验收证据。

## 六条最容易翻车的地方，提前说清楚

**1. `crates/kaleido-proto` 是合同，一个字段都不许改。**

不许改、不许加、不许删、不许在别处定义"临时的"平行类型。上一轮 T-014 就是因为在
proto 之前自建全局状态模型而被撤销。如果你认为协议有问题（很可能真的有），
按 `AGENTS.md` §5 停下来写报告交给主管，等批准。绕过合同的实现一律打回。
每次交付都要贴 `git diff --stat`，证明 proto 和 `docs/PROTOCOL.md` 零改动。

proto 里已经写好了一批校验器：`Turn::validate`、`QueueEntry::validate`、
`ContentRef::validate`、`CanonicalError::validate`、`LiveBinding::validate_against`、
`SessionSnapshot::validate`、`verify_contiguous`、`AttentionItem::check_reply`。
**它们必须在真实写路径上被调用**，不是只在测试里摆样子。

**2. 不许生成上游类型，也不许手写上游类型。**

`ADR-0012` 已经决定：`kaleido-adapter-codex` 不引入 typify/progenitor，也不定义任何与
`schemas/codex` 同名的类型（A-6 门禁会查）。做法是：

- 在 `surface.rs` 写一张**显式的钉定路径表**，每条含 canonical 用途、上游 method、
  JSON Pointer、以及 `schemas/required-surface.toml` 里对应的 `entries.id`；
- 解码器只读这张表里的路径，读到就立刻转成 canonical 类型；
- **未定型 JSON 不得离开 `kaleido-adapter-codex`**；
- 一个测试遍历该表，断言每条 pointer 在已提交 schema 快照里能解析、每个
  `entries.id` 存在、表里没有死条目。

不要为了"覆盖全"去把 275 个 schema 都读进来。只登记本卡实际用到的路径。

**3. 三条来自真实 fixture 的语义，错一条就是打回。**

这三条不是猜测，是本仓库已录到的一手证据（`docs/PROTOCOL.md` §11.2 给了行号）：

- **拒绝不是错误。** `04-permission-deny.jsonl` 里客户端回 `{"decision":"decline"}` 后，
  `fileChange` item 变成 `status: "declined"`，而整个 turn 仍然是 `turn/completed`。
  所以 canonical 里 `ItemStatus::Declined` 是正常终态，`Turn.status` 保持 `Completed`，
  `Turn.error` 为空。把 decline 映射成 `Failed` 或塞进 `ErrorCode` 都是打回项。
- **审批请求需要 join。** 审批请求的 params 只有 `threadId`/`turnId`/`itemId`，
  没有 diff 也没有命令内容；可展示的上下文在前一条 item 报文里。所以 `JoinState`
  必须实现，并且必须能处理"审批先到、item 后到"，中间态是
  `Unjoined { ItemNotYetSeen }` 且必须可渲染。
- **不要用 turn 结束报文重建 transcript。** `turn/completed` 带的是
  `itemsView: "summary"`，`items` 数组里**只有最后一条 agentMessage**，而那个 turn
  实际有 6 条 item。`Turn.item_ids` 必须由逐条 item 转移累积。

另外注意：服务端请求 ID 和客户端请求 ID 是**两个独立 ID 空间**——fixture 里客户端用
1/2/3，服务端的审批请求用 0。混在一张表里管会串。

**4. 队列不许假装成 steer，能力不许假装成支持。**

协议里**故意没有** steer 命令。引导只能表达为
`EnqueueInput { intent: SteerActiveTurn }`。只有当 runtime 返回指向当前活动 turn 的
注入确认（`SteerAcknowledgement.source == ObservedInTraffic`）时，条目才能变成
`DeliveredAsSteer`。Codex 这条能力**当前没有证据**，所以本卡里它必须一直是 `Pending`，
`RuntimeCapabilityView` 里 `turn_steer` 必须是 `not_verified` 或 `unsupported`。

同理：`CommandOutcome::AcceptedLocally` 不等于 `AcceptedByRuntime`；
能读历史不等于 `LiveBinding::Observing`；`CapabilityState` 缺项解析为 `NotVerified`，
不是 `Supported` 也不是 `Unsupported`。

**5. 测试必须能失败，而且要给我证据。**

每写完一个测试，把实现改坏一次，确认它变红。交付时**必须单独成节**给出至少三处
「改坏 → 变红」的实证，其中必须包含：

- decline 被错误映射为失败；
- 未经确认的 steer 被标记为已送达；
- 钉定 pointer 失效。

这三处在**阶段 A 的交付**里就要有。主管会自己挑一处你没报告的地方再改一次验证，
所以别只让那三处能红。

只有 happy path、`#[ignore]`、放宽断言、手写"理想上游报文"，一律打回。
契约测试必须用 `tests/fixtures/codex/` 下已提交的真实录制。

**6. 日志和输出不许泄漏。**

`docs/PROTOCOL.md` §10 列了必须走 `ContentRef` 的东西：消息正文、推理、工具参数与输出、
diff、**完整文件路径**、上游原始 ID、任何 token。
durable log 里只允许出现 canonical ID、枚举名、计数、`digest`、`byte_len`、时间戳、错误码。
本卡要求有一个测试真的去扫日志文件和 `tracing` 输出，断言这些东西不在里面。

## 关于 `cargo test --workspace`

主管已在下发前实跑过 `cargo xtask ci`，**exit 0，全绿**（proto 36/36、recorder 164/164、
fixtures 5 文件 220 条）。这就是你的基线：不得把任何历史失败重新列为允许失败，
也不得放宽生产脱敏守卫。你让哪个先前绿的测试变红，就是你的责任。

其余门禁必须全绿：`cargo fmt --all -- --check`、`cargo xtask check-deps`、
`cargo xtask lint-forbidden`、`cargo clippy --all-targets -- -D warnings`、
`cargo xtask fixtures verify`，以及四个新 crate 的测试。

一个已知坑：`cargo xtask check-deps` 用自己的 TOML 读取器解析 `crates/*/Cargo.toml`，
**不支持点号键**。`[package]` 里写 `edition = { workspace = true }`，
不要写 `edition.workspace = true`。照抄 `crates/kaleido-proto/Cargo.toml`。
每个新 crate 都要同时登记进根 `Cargo.toml` 的 `members` 和
`docs/dependency-rules.toml`——四个条目（`kaleido-state`、`kaleido-adapter`、
`kaleido-adapter-codex`、`kaleido-hostd`）主管都已经写好了，你只需要对齐依赖边，
不要去改规则文件。

## 不在本卡范围（写了就是超范围，会被要求删掉）

- Android / iOS / UniFFI / `kaleido-core` —— 归 [T-102](T-102.md) 和 R3，别碰
- OpenCode、Claude Code、ACP 的任何 adapter
- 传输层、配对、E2EE、relay、推送
- workflow 引擎与 `WorkflowBoardView`、`ProjectIndexView`
- 文件树、代码预览、Git
- 外部原生 CLI/GUI 的发现与附着（R7）
- 修 `xtask` 的 TOML 读取器（D-R1-1，单独开卡，别夹带）

## 遇到阻塞怎么办

**不要**绕过去、注释掉、改宽断言、改协议、擅自扩大范围。按 `AGENTS.md` §5 的格式报告，
把已完成部分和阻塞点讲清楚，等主管裁决。

特别地：如果真实 Codex 在本机因登录或网络问题跑不完一个 turn（历史上出现过），
那是**阶段 B** 的问题，阶段 A 的成果不受影响。登记阻塞、写明哪几格 DoD 缺一手证据，
**不要回头放宽阶段 A 的断言去凑绿**。

## 交付格式

按 `AGENTS.md` §4.2，两次交付各写一份：

1. **DoD 逐条勾选**，没做到的说明原因，不许含糊带过；
2. **粘贴真实测试输出**，不要只说"通过了"；
3. **`git diff --stat`**，证明 `crates/kaleido-proto/**`、`docs/PROTOCOL.md`、
   `docs/adr/**`、`schemas/**`、`tests/fixtures/**`、`spikes/**` 零改动；
4. **偏离说明** —— 任何与任务卡不一致的地方主动说明；
5. **发现的问题** —— 实现中察觉的协议/需求缺陷，即使不影响本任务也要报告；
6. **「改坏→变红」证据单独成节。**

现在开始阶段 A。先把 `docs/PROTOCOL.md` 和 `crates/kaleido-proto` 读完，再动第一行代码。

---

# 阶段 B（2026-07-31 放行）

> **换实现方 / 新会话请用冷启动版：[T-100-stage-b-codex-prompt.md](T-100-stage-b-codex-prompt.md)。**
> 下面这一节假定读者就是刚做完阶段 A 的那个实现方，带着上下文。

阶段 A 已通过评审：[docs/gates/T-100-stage-a-review.md](../gates/T-100-stage-a-review.md)。
主管做了独立复跑、三处你没报告过的变异（两处变红）、六个投影的实测抽查，
以及用文件修改时间为 `kaleido-proto` 未被触碰出具的证据。你的六处偏离全部被接受，
其中偏离 ① 和 ③ 是**你对、任务卡错**，已由
[ADR-0014](../adr/0014-codex-approval-families-and-timestamp-units.md) 和任务卡修订追平。

## 阶段 B 只做一件事

`slice run` 接真实 `codex app-server`，补齐：

- **§5.1 第四条**：真实 turn 进行中，`LiveActivityView` 至少被观察到一次非空 `streaming_item_ids`；
- **§5.3**：`--enqueue-steer` 的条目在 `InputQueueView` 中 `state = pending`、
  `steer_supported = false`，且**从未**出现 `delivered_as_steer`；
- **§5.4 第三条**：Codex 进程提前退出 → `ConnectionState = Unavailable { ProcessExited }`、
  `SessionStatus = offline`、`LiveBinding = NotBound { RuntimeExited }`，
  并产生一条 `ConnectionFault` 的 `AttentionItem`；
- **§6 真实验收**：三次运行的一手证据。

## 五条提醒

**1. 阶段 A 的断言一条都不许放宽。** 真实 Codex 因登录/网络跑不通，就按 §6.4 登记阻塞，
写明哪几格缺一手证据。回头改宽阶段 A 的断言去凑绿是最严重的违规。

**2. `turn_steer` 拿不到证据是正确结果，不是待修的缺陷。** 真实运行下它大概率仍然
只能是 `not_verified`。不要为了让它变 `supported` 去放宽 `SteerAcknowledgement` 的判定。
同理，真实运行下 `LiveBinding` 应该**能**变成 `Observing` 了——因为这次证据源真的是
`ObservedInTraffic`。这两件事的差别就是这张卡要证明的东西。

**3. P-1 不在阶段 B 范围内。** `AttentionState::Answered` 的 `command_id` 缺口已登记为
R3 前置，要改 `kaleido-proto`，必须单独开卡。这次真实运行里的审批决定是**本地发出的
命令**，所以它天然有真实 `command_id`——不要顺手去动 replay 那条路径的权宜实现。

**4. Windows 子进程纪律。** `CREATE_NO_WINDOW`；退出时杀整个进程树；
`--version` 先对齐 `schemas/codex` 快照，有漂移先 `cargo xtask schema diff` 并停下来报告。
工作目录用一次性的 `target/t100-scratch/`，**不要**用 `tests/fixtures/sandbox`。

**5. 审批场景的参数组合。** `approvalPolicy = on-request` + `sandbox = read-only`，
这是 03/04 两份 fixture 实际产生审批请求的配置。纯文本场景可用 `sandbox = workspace-write`。

## 交付

按 `AGENTS.md` §4.2，外加：

1. 六个投影各一份实际 JSON（脱敏后可直接贴）；
2. 运行 2 与运行 3 的 `fileChange` item 状态差异（`completed` vs `declined`），
   以及两者 `Turn.status` 均为 `completed`；
3. 运行 2 中 steer 条目始终 `pending` 的证据；
4. 至少一处新的「改坏 → 变红」，针对阶段 B 新增的代码路径（进程退出或 live binding）；
5. `git diff --stat` 自证禁改文件零改动。

主管仍然会自己挑你没报告的位置做变异。
