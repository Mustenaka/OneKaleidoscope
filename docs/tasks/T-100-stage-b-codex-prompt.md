# T-100 阶段 B — 下发给 Codex 的 prompt（冷启动版，原样粘贴）

> 这一份是给**没有任何上下文的新会话**用的。阶段 A 由另一个实现方完成，
> 你接手阶段 B。粘贴下面横线以内的全部内容。

---

你是 OneKaleidoscope 项目的**实现方（Implementer）**。仓库在 `D:\Work\Code\Cross\OneKaleidoscope`，开发机是 Windows 11。架构与协议由项目主管定义，你负责把任务卡变成可运行、可验证的代码。你的交付会被逐条审核，审核者会自己挑你没报告过的地方把实现改坏，验证测试是否真的变红。

**这个项目此前因为「先补齐上游 schema、再写产品代码」的排序停滞过一轮。现在的纪律是：一次只做一个纵切，每一条声明都要有机器证据。**

## 第一步：读，不要先写

按顺序全文读完再动第一行代码。这不是形式要求——协议是合同，你猜错的代价是整张卡重做。

| 文件 | 为什么必须读 |
|---|---|
| `AGENTS.md` | 你的行为规范。§2 铁律、§3 编码规范、§4 交付格式、§5 阻塞报告格式 |
| `docs/STATUS.md` | 当前状态、三条携带阻塞项 |
| `docs/tasks/T-100.md` | **本次任务卡。§5 的 Definition of Done 是唯一验收标准**。特别读 §5.0 的两阶段表 |
| `docs/gates/T-100-stage-a-review.md` | 阶段 A 的评审结论、被接受的偏离、新登记的协议缺口 |
| `docs/PROTOCOL.md` | 全文。你的所有类型都从它推导。重点 §4.2 能力、§4.3 会话与 live binding、§4.6 队列、§4.7 attention、§5 状态转移、§10 脱敏、§11 Codex 映射附录 |
| `crates/kaleido-proto/src/**` | 合同的代码形态 |
| `docs/adr/0012-provider-decode-strategy.md` | 你**不生成也不手写**上游类型，按钉定 JSON Pointer 表解码 |
| `docs/adr/0014-codex-approval-families-and-timestamp-units.md` | 只建模 file-change 一族审批；turn 时间戳是秒 |
| `docs/adr/0009-session-broker.md` | 三种会话所有权、能力五态、不依赖厂商私有 Remote Control |

然后**读懂阶段 A 已有的代码**（约 5000 行实现 + 1700 行测试），尤其是下面「你要接的三个缝」那一节点名的文件。

## 现状：阶段 A 已经完成，不要重写它

阶段 A 交付了离线 replay 纵切并已通过评审。`cargo xtask ci` 当前 **exit 0**。已存在的四个 crate：

```
crates/kaleido-state/          canonical 状态、durable log、内容寻址存储、六个投影、幂等
crates/kaleido-adapter/        provider 中立的 trait 与类型（无任何 Codex 概念）
crates/kaleido-adapter-codex/  钉定路径表 surface.rs / decode.rs / reduce.rs / transcript.rs
crates/kaleido-hostd/          组合根 + 诊断客户端
```

现有 CLI 只有两个离线子命令：

```
kaleido-hostd slice replay --fixture <path.jsonl> --log-dir <dir> [--base-at-ms <int>]
kaleido-hostd slice show   --log-dir <dir> --projection <name> [--session <id>]
```

`slice run` **不存在，也没有留桩**——这是阶段 A 的正确选择，你现在来实现它。

阶段 A 的测试是这张卡的回归网。**你让哪个先前绿的测试变红，就是你的责任。**不许放宽它的断言、不许 `#[ignore]`、不许改它的期望值来适配你的新代码。如果你确信阶段 A 某条断言错了，按 `AGENTS.md` §5 停下来报告，等主管裁决。

## 你的任务：只做阶段 B

把 `slice run` 接上真实的 `codex app-server` 进程，补齐 `docs/tasks/T-100.md` 里这四格：

- **§5.1 第四条**：真实 turn 进行中，`LiveActivityView` 至少被观察到一次非空 `streaming_item_ids`；
- **§5.3**：`--enqueue-steer` 放入的条目在 `InputQueueView` 中 `state = pending`、`steer_supported = false`，且**从未**出现 `delivered_as_steer`；`RuntimeCapabilityView` 里 `turn_steer` 是 `not_verified` 或 `unsupported` 并带 evidence；
- **§5.4 第三条**：Codex 进程提前退出时 → `ConnectionState = Unavailable { ProcessExited }`、`SessionStatus = offline`、`LiveBinding = NotBound { RuntimeExited }`，并产生一条 `ConnectionFault` 的 `AttentionItem`；
- **§6 真实验收**：三次真实运行的一手证据。

目标 CLI 形态（`--executable` 起，其余按卡里的写法）：

```
kaleido-hostd slice run --executable <codex.exe> --project-root <dir>
                        --log-dir <dir> --prompt <text>
                        [--decide-first-approval accept|decline]
                        [--enqueue-steer <text>] [--timeout-secs 120]
```

## 你要接的三个缝（都已经给你留好了）

架构已经把接入点定义清楚了，你不需要新发明抽象：

**1. `kaleido_adapter::session::ProviderRuntimeSession`**（`crates/kaleido-adapter/src/session.rs`）

provider 中立的 trait，已定义 `start` / `submit_prompt` / `respond_attention` / `drain_effects` / `close` / `capability_probe`。注意语义：**adapter 从不往 store 里推，组合根来拉**（`drain_effects`）。你在 `kaleido-adapter-codex` 里实现它。

**2. `CodexReducer::ingest_frame(&mut self, frame: &TranscriptFrame, ...)`**（`crates/kaleido-adapter-codex/src/reduce.rs`）

归约逻辑**已经写好并通过验证**。你要做的是把真实进程的双向报文包成 `TranscriptFrame`（`transcript.rs` 里有 `TranscriptFrame::from_wire`），喂给同一个 reducer。**不要为实时路径另写一套归约**——离线和在线必须共用同一个 decoder / reducer，这是这张卡要证明的东西之一。

**3. `ReducerConfig.evidence: EvidenceSource`**

这是 replay 与 live 的唯一开关。replay 用 `RecordedFixture`，所以 `LiveBinding` 恒为 `NotBound`。真实运行时你设 `ObservedInTraffic`，`LiveBinding` 就会变成 `Observing`——**这是正确的，也是阶段 B 要证明的**。别去改这个判定逻辑本身。

你需要新写的，主要是**进程传输层**：以子进程方式启动 `codex app-server`，走 stdio 双向 JSON-RPC，把收发报文转成 `TranscriptFrame`。以及 hostd 里的 `slice run` 编排。

## 六个最容易翻车的地方

**1. `crates/kaleido-proto` 是合同，一个字段都不许改。**

不许改、不许加、不许删、不许在别处定义「临时的」平行类型。proto 里的校验器（`Turn::validate`、`QueueEntry::validate`、`ContentRef::validate`、`LiveBinding::validate_against`、`SessionSnapshot::validate`、`verify_contiguous`、`AttentionItem::check_reply`）**必须在真实写路径上被调用**，不是只在测试里摆样子。

交付时贴 `git diff --stat` 自证 `crates/kaleido-proto/**`、`docs/PROTOCOL.md`、`docs/adr/**`、`schemas/**`、`tests/fixtures/**`、`spikes/**` 零改动。

**2. `turn_steer` 拿不到证据是正确结果，不是待修的缺陷。**

协议里**故意没有** steer 命令。引导只能表达为 `EnqueueInput { intent: SteerActiveTurn }`。只有当 runtime 返回指向当前活动 turn 的注入确认（`SteerAcknowledgement.source == ObservedInTraffic`）时，条目才能变成 `DeliveredAsSteer`。

Codex 这条能力当前没有证据。真实运行下它**大概率仍然**只能是 `not_verified`，队列条目**必须一直是 `pending`**。不要为了让它变成 `supported` 去放宽 `SteerAcknowledgement` 的判定——那是这张卡最想抓的作弊。

反过来：`LiveBinding` 在真实运行下**应该**能变成 `Observing`，因为证据源真的是 `ObservedInTraffic`。这两件事的差别，就是阶段 B 要证明的东西。

**3. 只建模 file-change 一族审批。**

`item/commandExecution/requestApproval` 与 `item/permissions/requestApproval` 的**回复合同不同构**（前者的 `decision` 是不同构的 `oneOf`，后者 required 是 `permissions` 而**根本没有 `decision`**）。按 ADR-0014，它们走 `UnknownUpstreamMessage`，**不得**渲染成审批项。给手机用户一个 runtime 不接受的按钮，比少一个按钮更糟。

**4. 不许生成、也不许手写上游类型。**

`surface.rs` 里那张钉定路径表是唯一的读值入口，未定型 JSON 不得离开 `kaleido-adapter-codex`。如果阶段 B 需要读新字段，**先往表里加登记项**（含 canonical 用途、上游 method、JSON Pointer、`schemas/required-surface.toml` 的 `entries.id`、schema anchor），让漂移守卫测试通过，再读。表里不许有解码器实际不读的死条目。

**5. Windows 子进程纪律。**

- 子进程必须 `CREATE_NO_WINDOW`；
- 退出时**杀整个进程树**，不要留孤儿 `codex` 进程；
- 平台专属代码只能出现在 `crates/*/src/platform/{windows,macos,linux}.rs`；
- 路径用 `directories` crate，不要手写 `%APPDATA%` 字面量；
- 注意 npm 装的是 `codex.cmd` 而不是 `codex`。

**6. 日志和输出不许泄漏。**

`docs/PROTOCOL.md` §10 列了必须走 `ContentRef` 的东西：消息正文、推理、工具参数与输出、diff、**完整文件路径**、上游原始 ID、任何 token。durable log 里只允许出现 canonical ID、枚举名、计数、`digest`、`byte_len`、时间戳、错误码。

阶段 A 已有一个扫描日志文件与 `tracing` 输出的测试（`crates/kaleido-hostd/tests/tracing_redaction.rs`）。它必须继续绿，而且**你新增的实时路径也要被它覆盖**。注意那个测试独占一个 test binary 是有原因的（`tracing` 的 callsite interest 是进程级缓存），不要把它并进别的文件。

## 真实验收（`docs/tasks/T-100.md` §6）

### 环境

Codex 精确版本 `0.146.0`（`codex-cli 0.146.0`），与 `schemas/codex` 快照一致。**先跑 `codex --version` 核对**；若本机已升级，先 `cargo xtask schema diff`，有漂移就停下来报告，不要拿新二进制对着旧快照跑。

工作目录**不要**用 `tests/fixtures/sandbox`。新建一次性目录 `target/t100-scratch/`，放一个 `editable.txt`（内容 `ORIGINAL`）。

审批场景的参数组合用 `approvalPolicy = on-request` + `sandbox = read-only`——这是 `03-permission-approve.jsonl` / `04-permission-deny.jsonl` 两份 fixture 实际产生审批请求的配置。纯文本场景可用 `sandbox = workspace-write`。

### 三次运行 + 一次重启

```powershell
$codex = "<绝对路径>\codex.exe"
$scratch = "target\t100-scratch"
$log = "target\t100-log"

# 1) 纯文本 turn
cargo run -p kaleido-hostd -- slice run --executable $codex --project-root $scratch `
  --log-dir "$log\a" --prompt "Reply with exactly this plain text and do not use any tool: KALEIDO T100"

# 2) 审批 -> accept，同时放一条 steer 意图输入
cargo run -p kaleido-hostd -- slice run --executable $codex --project-root $scratch `
  --log-dir "$log\b" --decide-first-approval accept --enqueue-steer "please also mention DONE" `
  --prompt "Use the file-editing tool to replace the complete contents of editable.txt with exactly KALEIDO T100 PROBE. Do not run a shell command."

# 3) 同一 prompt -> decline
cargo run -p kaleido-hostd -- slice run --executable $codex --project-root $scratch `
  --log-dir "$log\c" --decide-first-approval decline `
  --prompt "Use the file-editing tool to replace the complete contents of editable.txt with exactly KALEIDO T100 PROBE. Do not run a shell command."

# 4) 重启后从 durable log 重建
cargo run -p kaleido-hostd -- slice show --log-dir "$log\c" --projection all
```

### 允许的失败

如果真实 Codex 在本机因**登录或网络**不可用而无法完成 turn（历史上出现过），按 `AGENTS.md` §5 登记阻塞，写明哪几格 DoD 因此缺一手证据。

**但阶段 A 的全部离线 DoD 仍然必须绿。**不要为了凑绿伪造运行结果，也不要回头改宽阶段 A 的断言。

## 边界

**禁止修改：**

- `crates/kaleido-proto/**` —— 合同
- `docs/PROTOCOL.md`、`docs/adr/**`、`docs/REQUIREMENTS.md`、`docs/ARCHITECTURE.md`
- `schemas/**` —— 原样快照
- `tests/fixtures/**/*.jsonl` 与 `*.metadata.json` —— 一手证据
- `spikes/**` —— 冻结
- `xtask/**` 的规则语义

**禁止扩大到：**

- OpenCode、Claude Code、ACP 的任何 adapter
- 传输层、配对、E2EE、relay、推送
- Android / iOS / UniFFI / `kaleido-core`
- workflow 引擎、`WorkflowBoardView`、`ProjectIndexView`
- 文件树、代码预览、Git
- 外部原生 CLI/GUI 的发现与附着（`remoteControl/status/changed` 与 `codex app-server daemon` 只能作为诊断记录，**不得**作为依赖）
- 类型生成链（typify / progenitor）

**特别地：不要碰 P-1。** `AttentionState::Answered` 强制携带 `command_id` 是一个已登记的协议缺口（replay 路径下用了确定性铸造的 ID 作权宜）。它已排进 R3 前置，要改 proto 得单独开卡。阶段 B 里的审批决定是**本地发出的命令**，天然有真实 `command_id`，所以你根本不需要动那条路径。

## 工程门禁（全部必须绿）

```
cargo fmt --all -- --check
cargo xtask check-deps
cargo xtask lint-forbidden
cargo clippy --all-targets -- -D warnings
cargo test --workspace
cargo xtask fixtures verify
cargo xtask ci
```

已知坑：`cargo xtask check-deps` 用自己的 TOML 读取器解析 `crates/*/Cargo.toml`，**不支持点号键**。`[package]` 里写 `edition = { workspace = true }`，不要写 `edition.workspace = true`。新增 crate（如果你确实需要）必须同时登记进根 `Cargo.toml` 的 `members` 和 `docs/dependency-rules.toml`。

新增依赖要在交付说明里给出理由，优先选 workspace 里已有的 crate。

## 遇到阻塞怎么办

**不要**绕过去、注释掉、改宽断言、改协议、擅自扩大范围。按 `AGENTS.md` §5 的格式停下来报告：问题、影响、你的建议、备选方案、已完成部分。

## 交付格式

按 `AGENTS.md` §4.2，外加五项：

1. **DoD 逐条勾选**，没做到的说明原因，不许含糊带过；
2. **粘贴真实测试输出**，不要只说"通过了"；
3. **六个投影各一份实际 JSON**（脱敏后可直接贴），以及：运行 2 与运行 3 的 `fileChange` item 状态差异（`completed` vs `declined`）、两者 `Turn.status` 均为 `completed`、运行 2 中 steer 条目始终 `pending`、运行 4 与运行 3 结束时投影相同；
4. **至少一处新的「改坏实现 → 测试变红」实证**，针对阶段 B 新增的代码路径（进程退出、live binding、或 steer 判定），单独成节，贴原始红色输出；
5. **`git diff --stat`** 自证禁改文件零改动；以及**偏离说明**和**发现的问题**（协议/需求缺陷即使不影响本任务也要报告——阶段 A 就是这样发现了三处真实缺陷，其中两处是任务卡自己写错了）。

主管仍然会自己挑你没报告过的位置做变异复验。

现在开始。先把上面「第一步」的文件读完，再读阶段 A 的四个 crate，然后再写第一行代码。
