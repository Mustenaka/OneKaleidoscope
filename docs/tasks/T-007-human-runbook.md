# T-007: 真实录制 —— **人工执行手册**（不下发 Codex）

> 里程碑：M1 收尾
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
