# T-007: 真实录制

> 里程碑：M1 收尾
>
> **修订 R1（2026-07-30）**：负责人调整了网络参数，决定**先让 Codex 再试一次**。
> 因此本卡有两条执行路径：
>
> | 路径 | 执行人 | 何时用 |
> |---|---|---|
> | **A：Codex 重试** | Codex | 先走这条。**必须先做网络连通性预检，不通就立刻停**，不许再烧一整轮 |
> | **B：人工执行** | 负责人本人 | 路径 A 的预检未通过时，直接转这条 |
>
> 路径 A 的预检要求见文末「§路径 A：Codex 重试的预检门」。
> 以下正文原为人工手册，两条路径的录制步骤与优先级完全相同。
>
> **路径 A 执行结果（2026-07-30）：P-1 未通过，已转路径 B。**
>
> ```
> curl.exe --fail --location --silent --show-error --max-time 15 --range 0-127 \
>   https://raw.githubusercontent.com/.../schema/v1/meta.json
> curl: (7) Failed to connect to raw.githubusercontent.com:443 after 6 ms
> ```
>
> **6 毫秒即失败，P-2~P-4 未执行，未写任何代码。** 预检门达到设计目的：
> 前两轮各消耗一整轮工作量才得出同一结论，本轮成本为一条命令。
>
> 判定：负责人调整的是**系统网络参数**，而 Codex 的沙箱在**进程层**拦截出站连接，
> 两者是不同的层。`Could not connect` 在 6ms 内返回是本地拦截的特征（真实网络故障
> 通常表现为超时或 DNS 失败，不会这么快）。

> 执行人：**项目负责人本人**，在自己的交互式 PowerShell 里
> 前置：T-006 已交付（录制器已通过审核，可用）
> 预计耗时：20~40 分钟（取决于三家的响应速度）

---

## 为什么这张卡不给 Codex

T-004 与 T-006 两轮都卡在同一处，根因现已完全查清 —— **Codex 的执行环境里有三条互相独立的障碍，任何一条都足以让录制失败**：

| # | 障碍 | 证据 |
|---|---|---|
| 1 | **出站网络被封** | OpenAI WebSocket/HTTPS 不可达；GitHub raw 不可达；OpenCode POST 超时；`cargo` 需离线 vendoring。三家全部卡在**第一次出站 API 调用**，而本地部分（`initialize` / `thread/start` / `session/new`）全部成功 |
| 2 | **解析到的是另一个二进制** | `where.exe codex` 命中 `C:\Program Files\WindowsApps\OpenAI.Codex_…\app\resources\codex`（Store GUI 版），该实例返回 `Not logged in`；负责人终端里的 CLI 版是 `Logged in using ChatGPT`。**两套独立登录态**（R-13） |
| 3 | **读不到用户配置** | OpenCode 用隔离 XDG，落到 `openai/gpt-5.6`，读不到负责人配的 DeepSeek v4 |

补充：`where.exe node` 也是未命中，尽管 `C:\nvm4w\nodejs` 在 PATH 里 —— nvm symlink 当前进程无权访问。

**再发第三轮任务卡是浪费。** 录制器本身已经通过审核，它缺的不是代码，是一个**有网络、有登录态、有真实配置**的进程。那个进程就是你的 PowerShell。

---

## 你要做的

### 第 0 步：确认起点

在你自己的 PowerShell（就是那个显示 `(base) PS ...` 的）里，仓库根目录：

```powershell
cd D:\Work\Code\Cross\OneKaleidoscope
codex login status
node --version
opencode --version
```

三条都要有正常输出。**如果 `codex login status` 显示已登录，记下 `where.exe codex` 的第一行是哪个路径** —— 后面要用它，而不是让录制器自己去找。

### 第 1 步：先跑一个场景验证通路

不要一上来跑 27 个。先验证最简单的一个：

```powershell
cargo run -p kaleido-recorder -- codex simple-turn --timeout-secs 120
```

- **成功**（生成 `tests/fixtures/codex/01-simple-turn.jsonl`）→ 进第 2 步
- **失败** → 把完整输出发我，不要自己往下试

如果录制器自动发现选错了二进制，用显式路径覆盖（发现优先级①）：

```powershell
cargo run -p kaleido-recorder -- codex simple-turn --executable "<你上面记下的 codex 路径>" --timeout-secs 120
```

### 第 2 步：按优先级录

**P0 最重要 —— 权限审批是 M2 的卡点（R-8）。先录这四个：**

```powershell
cargo run -p kaleido-recorder -- codex permission-approve --timeout-secs 180
cargo run -p kaleido-recorder -- codex permission-deny --timeout-secs 180
cargo run -p kaleido-recorder -- opencode permission-approve --timeout-secs 180
cargo run -p kaleido-recorder -- opencode permission-deny --timeout-secs 180
```

> 录 `permission-*` 时录制器会等一个真实的审批请求。这些场景需要 agent 真的去动文件或跑命令，
> 所以耗时会比 `simple-turn` 长，`--timeout-secs 180` 是有意放宽的。

**P1 —— 工具调用与 diff：**

```powershell
foreach ($a in "codex","opencode","acp") {
  foreach ($s in "tool-call","file-change") {
    cargo run -p kaleido-recorder -- $a $s --timeout-secs 180
  }
}
```

**P2 —— 其余场景：**

```powershell
foreach ($a in "codex","opencode","acp") {
  foreach ($s in "simple-turn","error","cancel","session-load") {
    cargo run -p kaleido-recorder -- $a $s --timeout-secs 180
  }
}
```

**P3 —— elicitation（范围已缩小，见 [ADR-0007](../adr/0007-elicitation-capability-gated.md)）：**

```powershell
cargo run -p kaleido-recorder -- codex elicitation --timeout-secs 180
cargo run -p kaleido-recorder -- opencode elicitation --timeout-secs 180
```

Claude ACP 不用录 —— 钉定的 ACP v1 schema 里根本没有 elicitation。

> **OpenCode 的 elicitation 提示**：你既有的 fixture 里有
> `{"permission":"question","pattern":"*","action":"deny"}`。
> `question` 很可能就是 OpenCode 的 elicitation 等价物，但被设成了 `deny`。
> 录之前把它改成允许。

### 第 3 步：校验并交回

```powershell
cargo run -p xtask -- fixtures verify
cargo run -p xtask -- ci
```

然后把这些发给我：

1. `cargo run -p xtask -- fixtures verify` 的输出（会显示各家录到几条）
2. `tests/fixtures/` 下新增了哪些文件（`git status --short tests/`）
3. 任何失败场景的完整报错

---

## 关于 Claude ACP 的 120 秒超时

这一条**可能在你的环境里自然消失**（如果根因是网络）。但如果在你的终端里仍然超时，那就是独立问题，需要单独定位。

先手工验证一次 —— ACP 是 stdio JSON-RPC server，直接跑它不会有输出，必须喂 `initialize`：

```powershell
'{"id":1,"jsonrpc":"2.0","method":"initialize","params":{"clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":true},"clientInfo":{"name":"probe","version":"0.1.0"},"protocolVersion":1}}' | npx.cmd --yes "@agentclientprotocol/claude-agent-acp@0.63.0"
```

应当在几秒内回一个含 `agentCapabilities` 的 JSON。**能回就说明进程是好的**，超时出在后面的模型调用上，那是网络或账号维度的问题。

---

## 录不到也没关系

**不要为了填满表格而勉强。** 只要 P0 的四个里能录到**任意两个**（一家批准 + 一家拒绝），R-8 就有了可比较的一手形状，M2 就能推进。

录不到的如实告诉我卡在哪一步，我按现有 schema 证据先定协议骨架，字段可选性留待后补。

---

## 安全提醒

录制会把真实报文写进 `tests/fixtures/`。脱敏链路已经通过审核（用户名、家目录、`sk-`/`ghp_`/`Bearer`、沙盒外路径、命令清单都会被替换），但**提交前请自己扫一眼新增的 fixture**，尤其是 `03`/`04` 这两个 —— 它们包含工具调用参数，是最容易带出真实内容的地方。

`cargo run -p xtask -- fixtures verify` 会拦住已知模式，但它拦不住它不认识的东西。

---

## §路径 A：Codex 重试的预检门（**先做这个，不通就停**）

前两轮各烧掉一整轮工作量才得出「环境不通」的结论。这次不许重复。

### 预检（限时，不写任何代码）

按顺序做完这四项，**全部通过才允许进入录制**：

| # | 检查 | 通过标准 |
|---|---|---|
| P-1 | 出站 HTTPS 连通性 | 能取到 `https://raw.githubusercontent.com` 的任意内容（前两轮此处不可达） |
| P-2 | 选中的 codex 二进制及其登录态 | 打印**实际选中的路径**与 `login status` 结果。命中 `WindowsApps\OpenAI.Codex_*` 且返回 `Not logged in` 即为 **未通过**（R-13：那是 Store GUI 版，与用户 CLI 版登录态不同） |
| P-3 | OpenCode provider 可达 | 用**用户真实配置**（不要隔离 XDG）起 server，确认选中的 provider 不是隔离环境下的 fallback |
| P-4 | 一个最小真实调用 | 跑 `codex simple-turn`，**能拿到模型返回的文字**即通过 |

### 预检不通过时

**立即停止，按 `AGENTS.md §5` 报告，不要开始录制、不要改代码、不要重试其他 agent。**
报告只需说明：哪一项没过、原始输出是什么。主管会转路径 B。

### 关于 P-2 的补充要求

如果自动发现选到了 Store GUI 版，**不要就此判定失败** —— 先用发现优先级①
（显式指定路径）指向用户已登录的 CLI 版再试。这本身也是 `ARCHITECTURE.md §6.1`
要求的行为：发现是在候选中做选择，选择依据包含登录态。

### 预检通过后

按正文的 P0 → P3 优先级录制。**P0（三家的 `permission-approve` / `permission-deny`）
是唯一真正要紧的** —— 只要拿到任意两家（一家批准 + 一家拒绝），R-8 就够主管定协议了。

其余场景录不到就如实填表，不要为了填满而勉强。
