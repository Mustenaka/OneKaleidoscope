# T-004 / T-006 / T-009：三家 agent 的真实协议录制

录制日期：2026-07-29。所有录制进程的工作目录都是本目录下的 `sandbox/`
玩具项目；没有主动在 OneKaleidoscope 仓库根目录或其他真实项目中启动录制。
一次早期 OpenCode session-load 虽从 sandbox 启动，却自行把父仓库识别为项目根；
该临时输出只含项目根元数据和固定 seed prompt，不含源码，已立即丢弃。提交的
OpenCode fixture 是用临时嵌套 toy repository 重新录制的，原始 `directory`、
`cwd`、`root` 均精确等于 sandbox。

T-006 在同日完成环境修复后的补录尝试，但没有新增达到场景判据的 fixture。
下文严格区分“已提交的契约 fixture”和“只用于诊断的失败尝试”：后者即使包含
真实协议事件，也不计入覆盖率，构建目录中的临时日志也不作为契约 fixture。

## 证据完整性

- 只提交达到目标场景判据的真实 transcript。失败尝试会写在下方矩阵中，
  不把初始化、健康检查或与目标无关的事件包装成成功 fixture。
- 当前成功样本有两份：
  `acp-claude/06-cancel.jsonl` 来自
  `@agentclientprotocol/claude-agent-acp@0.63.0` 实际进程；
  `opencode/08-session-load.jsonl` 来自 `opencode-ai@1.18.8`
  实际 HTTP 服务读取其真实持久化会话。两份都不是手写报文。
- 每行 `payload` 的键顺序和字段均按线上原始 JSON 保留。录制器只在原始
  JSON token 上做下节列出的确定性脱敏，不解析后重新序列化 payload。
- 失败尝试先写入同目录临时文件；只有目标协议证据完整时才原子安装为
  `.jsonl`。因此覆盖表不会引用失败尝试的临时 transcript。
- 成功判据按同一 session/turn/item 或 toolCallId 关联完整生命周期：
  tool call 必须有 start、非空 update、terminal；file-change 必须有实际文本
  变化；权限场景必须在回复决策后看到同一目标的终态与 turn/prompt 收口。
- 场景完成判定会检查活动 turn/prompt 的全部 lifecycle，而不只挑一个看起来正确
  的 call：孤儿 update/terminal、冲突 ID、额外 call、范围无法证明的 call 都会让
  整个尝试失败。simple-turn 与 elicitation 也不能夹带工具生命周期。
- `acp-claude/06-cancel.jsonl:6` 的 `available_commands_update` 原始载荷含
  负责人本机 58 条 slash command / skill / 插件配置。按 T-006 主管批准的唯一
  payload 例外，该数组已由同一脱敏链替换成保留原长度的单项摘要；其余字段和
  cancel 生命周期仍逐字保留。
- 三家录制入口都会拒绝 sandbox 根本身是 symlink/junction/reparse point；
  OpenCode session-list 的原始响应在确认所有会话目录均为 sandbox 且存在精确
  seed 前只保留在内存，不会把 outside 元数据写进临时 fixture。
- 每次运行会先把原始 sandbox 原子重命名到同父目录的私有守卫位置，再复制出
  agent 可写的工作副本。结束时先隔离工作副本，再把原始目录原子改名回来；
  因此 ACL、hidden/readonly 位、Unix mode 和 hardlink 拓扑不会被“复制恢复”
  悄悄改写。根目录或子项出现 link/reparse、守卫无法恢复、或输出目录逃出
  workspace 时均 fail-closed。
- 三家权限响应只接受能规范化到当前 sandbox 内的真实结构化资源。外部绝对路径、
  `..` traversal、Windows drive/UNC/drive-relative、junction/reparse、占位符、
  无法解析的命令以及除精确 `cargo run` / `cargo run --` 之外的 shell 命令都会
  在回复 agent 和写 fixture 前被拒绝。

## 录制清理元数据

新录制在严格场景判据通过后，会把 transcript 安装为 `.jsonl`，并同时安装同名
`.metadata.json` sidecar（例如 `01-simple-turn.metadata.json`）。现有历史 fixture
早于该元数据约定，不追溯补写。sidecar 字段含义如下：

| 字段 | 取值与含义 |
|---|---|
| `cleanup` | `"complete"`：未观察到录制后清理错误；`"incomplete"`：transcript 已合格，但至少一项进程清理无法确认完成 |
| `unconfirmed_pids` | 排序、去重后的未确认终止 PID；进程在新的 Toolhelp32 快照中已消失时不会列入 |
| `cleanup_errors` | 清理阶段的原始诊断列表；写入前仍经过与 fixture 相同的确定性脱敏 |

`cleanup: "incomplete"` 会在 stderr 打印醒目警告，但合格 transcript 仍安装且
recorder 返回退出码 0。清理状态不参与场景合格判定；工具生命周期、权限决策、
session/turn 关联等 transcript 判据没有放宽。不满足判据的临时 transcript 仍会
在安装前作废，即使进程清理完全成功也不会产生 `.jsonl` 或 metadata。

## 确定性替换规则

替换按固定顺序执行；同一个原串在所有文件中得到同一个占位符。路径、用户名、
密钥前缀与敏感字段名按 ASCII 大小写不敏感匹配；从下表环境变量读取的凭据值
只做大小写敏感的逐字匹配，避免把普通文本误当成凭据。

| 原内容 | 占位符 |
|---|---|
| `directories::BaseDirs` 返回的家目录（同时覆盖 `\` 与 `/` 形式） | `<HOME>` |
| `USERNAME` / `USER` 的值 | `<USER>` |
| `tests/fixtures/sandbox` 的绝对路径（同时覆盖 `\` 与 `/` 形式） | `<SANDBOX>` |
| 以 `sk-`、`ghp_`、`Bearer ` 开头的值 | `<REDACTED_TOKEN>` |
| JSON 字符串字段 `api_key`、`authorization` 的值 | `<REDACTED_TOKEN>` |
| 支持的凭据环境变量的非空 Unicode 值：Anthropic/Claude、AWS Bedrock、Azure、Codex/OpenAI、Gemini/Google、GitHub、OpenRouter、OpenCode。长度至少 8 时逐字替换；短值只在它恰好是完整 JSON 字符串值时替换 | `<REDACTED_TOKEN>` |
| JSON 字符串中出现的沙盒外绝对文件路径；为防止命令串残留上下文，替换整个字符串 token。HTTP envelope 顶层 `path` 是路由而非文件路径，仍会扫描其 query 中的密钥 | `<OUTSIDE_PATH>` |
| 仅当 method=`session/update` 且 discriminator=`available_commands_update` 时，精确路径 `params.update.availableCommands` 的原始数组 | `[{"name":"<REDACTED_COMMAND>","description":"<REDACTED_COMMAND>","_meta":{"kaleidoRedaction":"available_commands_update","originalCount":N}}]`；`N` 是原数组长度 |

脱敏后再次执行泄漏扫描；命中当前用户名、家目录、密钥前缀、授权头或沙盒外
绝对路径都会拒绝落盘。用户名只在 JSON 值中检测，键名恰好等于用户名不会误报；
`sk-` / `ghp_` 需要位于 token 边界，`task-based`、`flask-test` 等普通单词不会
被修改或误报。对象键属于上游结构，因此用户名同名键保持原样；但键本身若是含
`/` 或 `\`、且指向沙盒外的路径样式字符串，仍由路径扫描拒绝。

命令清单规则只处理上述精确 ACP discriminator；缺失、非数组或重复的
`availableCommands` 会在写任何 fixture 字节前失败。既有样本通过同一规则原子
重处理，不手工改 JSON：

```powershell
cargo run -p kaleido-recorder -- redact-fixture acp-claude/06-cancel.jsonl
```

该命令只接受三家目录下九个规范场景文件名，全部记录处理成功后才替换原文件。
对处理后的文件再次运行是字节幂等的。本次文件由 23,214 bytes 降为
4,619 bytes（下降约 80.1%），摘要中的 `originalCount` 仍为 58。

## 精确版本

| 接入 | 实际录制运行时 | 对照 schema |
|---|---|---|
| Codex | `@openai/codex@0.146.0`，原生二进制报告 `codex-cli 0.146.0` | `schemas/codex`：`codex-cli 0.146.0`，275 个原样 JSON |
| Claude Code / ACP | `@agentclientprotocol/claude-agent-acp@0.63.0`；其 npm 自备原生二进制报告 `2.1.220 (Claude Code)` | `agent-client-protocol-json-schema-v1@1.18.0`，wire v1 |
| OpenCode | `opencode-ai@1.18.8` | `schemas/opencode/openapi.json`：OpenAPI 3.1.0，CLI `1.18.8` |

### Codex 0.146.0 的 schema 漂移

录制前先对旧的 Codex 0.144.6 snapshot 执行语义 diff。命令精确抓取
Codex 0.146.0、OpenCode 1.18.8 与 ACP crate 1.3.0 / schema 1.18.0，并报告：

```text
schema: fetching Codex 0.146.0, OpenCode 1.18.8, ACP crate 1.3.0 / schema 1.18.0
schema: used a configured ACP snapshot verified against commit 48b2abf1ac750fece26e03e92e773ccbd4754f5d
xtask: schema drift detected at 2380 path(s)
```

2,380 条漂移全部属于 `codex/`；OpenCode 与 ACP 没有漂移路径。代表性变化包括
新增 Apps read/installed 请求、新增音频 content variant、thread `isPinned`、
`RawResponseCompletedNotification`，以及 `UserInput` 联合类型变化。这不是键顺序
噪声。随后按任务卡授权执行 schema refresh；`schemas/VERSIONS.md` 与
`schemas/codex` 当前都已钉到 0.146.0。refresh 后用同一精确工具版本与经 Git
blob 校验的 ACP commit snapshot 复跑，结果为
`schema diff: no semantic drift (278 JSON files compared)`。

## 本轮实际 model / provider

| 接入 | 实际证据与结论 |
|---|---|
| Codex | 录制器没有在 `thread/start` 中覆盖 model/provider。负责人交互终端已确认 ChatGPT 登录，但本次使用的精确 0.146.0 二进制在录制进程环境中报告 `Not logged in`；同时 WebSocket/HTTPS 连接在任何 agent message 前失败。因此没有证据证明该进程复用了交互登录态，也没有足够 wire 证据给出实际采样模型名。 |
| Claude ACP | `session/new` 返回的模型配置仍是 `currentValue:"default"`，候选说明把 default 映射为 Sonnet 5，initialize 的 `providers` 为空。由于所有补录 prompt 都没有返回模型事件，不能把说明文字升级为“已观察到某个具体 Claude 模型/provider”。 |
| OpenCode | 八个最终非 session-load 场景实际选择 `providerID=openai`、`modelID=gpt-5.6`。隔离 XDG 配置是为安全录制而新建的；当前受限进程无法读取负责人终端里已配置的 DeepSeek v4，因此没有复用该真实配置。这是相对任务卡“本轮已改为 DeepSeek v4”的明确偏离，不是把旧的 Gemini 结论延续到本轮。 |

## 录制沙盒状态

每次录制前必须保持：

```text
tests/fixtures/sandbox/
  Cargo.toml
  README.md
  editable.txt       # 内容为 ORIGINAL
  notes.txt
  src/main.rs        # 支持正常、fail、wait 三条确定性路径
```

录制器在每次尝试结束后原子恢复整个 sandbox，并删除工作副本中本次尝试创建的
`permission-probe.txt`。可用下面的命令检查状态：

```powershell
git diff --exit-code -- tests/fixtures/sandbox
Test-Path tests/fixtures/sandbox/permission-probe.txt
```

第二条必须输出 `False`。

T-006 最终尝试的清理检查只跟踪 recorder 本次 spawn 的根 PID 及其家族，不再按
`codex.exe`、`claude.exe` 或 `opencode.exe` 名称扫描全系统进程。最终 Codex、
ACP 与 OpenCode 尝试结束后均成功回收本次 spawn 的进程家族并恢复原始 sandbox；
OpenCode 的八个 120 秒最终尝试也都完成服务清理和 restore，没有因机器上其他
agent GUI/CLI 正在运行而误判失败。恢复后 `editable.txt` 仍为 `ORIGINAL`，
`permission-probe.txt` 不存在，临时嵌套 `.git` 也不在提交树中。

## Windows 多源发现

先运行发现命令。它会输出进程自身 `PATH` 的完整值，并按固定优先级逐层列出
每个候选、候选状态、`where.exe` 原始输出、实际 `--version` 探测结果和最终
来源；不会把“当前 PATH 没找到”推断成“未安装”。

```powershell
$repo = (Resolve-Path .).Path
$codex = (Resolve-Path "$repo\target\npm-cache-t006\_npx\1c016cf0cb57261d\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe").Path
$opencode = (Resolve-Path "$repo\target\npm-cache\_npx\78a1f4be48eedf14\node_modules\opencode-windows-x64-baseline\bin\opencode.exe").Path
$claudeAcp = (Resolve-Path "$repo\target\npm-cache\_npx\3e28e223a0aba92d\node_modules\.bin\claude-agent-acp.cmd").Path
$bundledClaude = (Resolve-Path "$repo\target\npm-cache\_npx\3e28e223a0aba92d\node_modules\@anthropic-ai\claude-agent-sdk-win32-x64\claude.exe").Path
cargo run -p kaleido-recorder -- discover --codex $codex --opencode $opencode --bundled-claude-acp $claudeAcp --bundled-claude $bundledClaude
```

本次执行的逐层结论如下。路径在工具输出中先经过与 fixture 相同的脱敏器。

| 目标 | ① 显式配置 | ② 继承 PATH | ③ 持久化 PATH | ④ 已知位置 | ⑤ hostd 自备 |
|---|---|---|---|---|---|
| Codex | 精确的 `0.146.0` 原生 exe，可运行并被选中 | 解析到 WindowsApps GUI 内 `codex.exe` | 未解析到可运行 CLI | 找到 GUI/应用数据目录中的原生 exe；目录证据本身不等于 CLI 判定 | 不适用 |
| Claude ACP | 未配置 | 未解析到 launcher | 未解析到 launcher | 找到 Node/npm 安装位置；不据此自动安装 ACP 包 | 已安装的 `claude-agent-acp.cmd`，`0.63.0`，可运行并被选中 |
| Claude CLI | 未配置 | 未解析到 launcher | 未解析到 launcher | 只有 nvm/npm 安装目录证据，没有可运行候选 | 不适用；ACP 首选路径不依赖此 CLI |
| OpenCode | 精确的 `1.18.8` 原生 exe，可运行并被选中 | 未解析到 launcher | 未解析到 launcher | 只有 npm/nvm 安装目录证据，没有可运行候选 | 不适用 |
| Node | 精确的 `node.exe`，可运行，`v22.13.0`，并被选中 | nvm symlink 存在但当前进程无访问权 | 未解析到可运行 Node | 注册表安装位置的同版本 `node.exe` 也可运行 | 不适用 |

本次进程 `PATH` 对五个目标相同；以下是 2026-07-29 实际输出，用户名已经由
发现工具按 fixture 规则替换。它含 GUI Codex 和 `C:\nvm4w\nodejs` 条目，
但不含交互 PowerShell profile 才注入的完整环境；其中 nvm 条目在当前隔离
进程中返回 `PermissionDenied`。

```text
process_PATH=D:\Work\Code\Cross\OneKaleidoscope\target\debug\build\aws-lc-sys-7891f649ffa749de\out;D:\Work\Code\Cross\OneKaleidoscope\target\debug;D:\Work\Code\Cross\OneKaleidoscope\target\debug\deps;C:\Users\<USER>\.rustup\toolchains\1.94.0-x86_64-pc-windows-msvc\lib\rustlib\x86_64-pc-windows-msvc\lib;C:\Users\<USER>\.codex\tmp\arg0\codex-arg0iyMczQ;C:\Users\<USER>\.cache\codex-runtimes\codex-primary-runtime\dependencies\bin\override;C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.6\bin;C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v12.6\libnvvp;C:\Program Files\NVIDIA\CUDNN\v9.5\bin;C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v11.8\bin;C:\Program Files\NVIDIA GPU Computing Toolkit\CUDA\v11.8\libnvvp;C:\Program Files\Common Files\Oracle\Java\javapath;D:\Program Files (x86)\VMware\VMware Workstation\bin\;C:\WINDOWS\system32;C:\WINDOWS;C:\WINDOWS\System32\Wbem;C:\WINDOWS\System32\WindowsPowerShell\v1.0\;C:\WINDOWS\System32\OpenSSH\;C:\Program Files\Git\cmd;C:\Program Files (x86)\NVIDIA Corporation\PhysX\Common;C:\Program Files\CMake\bin;D:\Program\sqlite\sqlite-tools-win-x64-3460100;C:\Program Files\Docker\Docker\resources\bin;C:\ProgramData\anaconda3;C:\ProgramData\anaconda3\Scripts;C:\ProgramData\anaconda3\Library\bin;C:\ProgramData\anaconda3\Library\mingw-w64\bin;D:\Program Files\MATLAB\R2024b\bin;C:\Program Files\NVIDIA Corpora;d:\Program Files\cursor\resources\app\bin;C:\Program Files\dotnet;C:\Program Files\dotnet\;C:\Program Files\NVIDIA Corporation\NVIDIA app\NvDLISR;D:\Program\ngrok\ngrok-v3-stable-windows-amd64;C:\Program Files\Git\mingw64\bin;D:\Program Files\MVTec\HALCON-20.05-Progress\bin\x64-win64;C:\Program Files (x86)\Windows Kits\10\Windows Performance Toolkit\;C:\Program Files\Go\bin;D:\MySoftware\ffmpeg\ffmpeg-2025-12-18-git-78c75d546a-full_build\bin;D:\MySoftware\Redis-8.4.0-Windows-x64-msys2-with-Service;D:\MySoftware\minio;C:\Program Files\Pandoc\;C:\Program Files\Java\jdk-22\bin;D:\MySoftware\flutter\flutter_windows_3.41.4-stable\flutter\bin;C:\Program Files\NVIDIA Corporation\Nsight Compute 2024.3.0\;D:\Program Files\PostgreSQL\17\bin;C:\Users\<USER>\.local\bin;C:\Users\<USER>\.cargo\bin;C:\Users\<USER>\AppData\Local\Microsoft\WindowsApps;D:\Program Files\Microsoft VS Code\bin;C:\Users\<USER>\go\bin;C:\Users\<USER>\AppData\Local\Programs\Ollama;C:\Users\<USER>\.dotnet\tools;C:\Users\<USER>\AppData\Local\JetBrains\Toolbox\scripts;C:\Users\<USER>\AppData\Local\Microsoft\WindowsApps;C:\Users\<USER>\AppData\Roaming\npm;C:\Users\<USER>\AppData\Local\nvm;C:\nvm4w\nodejs;D:\Program Files\cursor\resources\app\bin;C:\Users\<USER>\.dotnet\tools;C:\Users\<USER>\go\bin;C:\Users\<USER>\.cache\codex-runtimes\codex-primary-runtime\dependencies\bin\fallback;C:\Users\<USER>\.cache\codex-runtimes\codex-primary-runtime\dependencies\native\git\cmd;C:\Users\<USER>\AppData\Local\OpenAI\Codex\bin\3e42d49ad3e35a50;C:\Program Files\WindowsApps\OpenAI.Codex_26.721.4979.0_x64__2p2nqsd0c76g0\app\resources;C:\Users\<USER>\.rustup\toolchains\1.94.0-x86_64-pc-windows-msvc\bin
```

同一次探测的 `where.exe` 原始输出如下；`exit=1` 是未命中，不等价于未安装：

```text
where.exe codex (exit=0):
C:\Program Files\WindowsApps\OpenAI.Codex_26.721.4979.0_x64__2p2nqsd0c76g0\app\resources\codex
C:\Program Files\WindowsApps\OpenAI.Codex_26.721.4979.0_x64__2p2nqsd0c76g0\app\resources\codex.exe

where.exe claude-agent-acp (exit=1):
INFO: Could not find files for the given pattern(s).

where.exe claude (exit=1):
INFO: Could not find files for the given pattern(s).

where.exe opencode (exit=1):
INFO: Could not find files for the given pattern(s).

where.exe node (exit=1):
INFO: Could not find files for the given pattern(s).
```

登录态是独立维度，不能由安装/发现状态推断。本次安全探测只检查受支持凭据
环境变量是否存在且非空，不输出其内容；得到：

| 目标 | 鉴权结论 | 证据 |
|---|---|---|
| Codex 0.146.0 | `interactive-auth-confirmed; recorder-process-not-authenticated` | 人类前置检查在负责人的交互终端返回 `Logged in using ChatGPT`，但本次精确二进制在录制进程环境中返回 `Not logged in`；同时 app-server 与 CLI seed 的 OpenAI WebSocket/HTTPS 均不可达。不能把交互登录态外推为本进程已复用 |
| Claude ACP / npm 自备 Claude | `authenticated` | 自备 `claude auth status --json` 返回 `loggedIn=true` |
| 用户 Claude CLI | `inconclusive` | 本进程没有可运行候选；不能据此说未安装或未登录 |
| OpenCode 1.18.8 | `isolated-provider-observed-network-blocked` | 最终八次补录实际选择 `openai/gpt-5.6`，但当前受限进程无法连接 provider API。负责人交互终端里的 DeepSeek v4 配置无法从该隔离进程读取；不能据此推断负责人未配置或未登录 |
| Node | `not-applicable` | 运行时前置，没有 agent 登录态 |

Windows 解析按 `PATHEXT` 顺序只尝试允许的 `.cmd` / `.exe` / `.bat`，不会执行 npm
生成的无扩展 POSIX sh shim；`.cmd` 经 `cmd.exe /D /S /C` 启动，所有子进程
使用 `CREATE_NO_WINDOW`。终止时优先执行 `taskkill /PID ... /T /F` 杀进程树；
若 `taskkill` 启动失败或非零退出，则回退到直接 `child.kill()` 并等待回收。
受限宿主下的回退不能保证已脱离句柄的后代进程也被终止，因此调用返回明确错误，
不会把“只杀了根进程”报告为整树清理成功。已知 npm 位置只做静态枚举；
除了平台默认位置和 `NPM_CONFIG_PREFIX`，还会读取用户 `.npmrc` 中最后一个精确
`prefix=` 设置，并支持 `${HOME}` / `${USERPROFILE}` / `${APPDATA}` / `~` 的
机械展开。读取错误只记录错误种类，其他 npm 配置和值（尤其 auth token）绝不进入
诊断。当前无法保证 cmd 子孙进程回收时，不运行动态 `npm prefix --global`，诊断会
明确写出这一限制。

Node 不是只做“存在性”检查：多源发现选出的 Node 必须先通过 `--version` 探测，
随后其父目录会在 Claude ACP `.cmd --version` 候选探测前置于该子进程 PATH；
同一个已验证 adapter 句柄再用于实际录制 spawn。这样显式 Node 位于继承 PATH、
注册表和 npm 默认位置之外时也不会出现发现与启动结论不一致。该 PATH overlay
只绑定 Claude ACP 子进程，不修改 recorder 自身环境，也不影响 Codex/OpenCode。

## 可复现命令

以下命令均从仓库根目录运行。任务目录内的 npm cache 是一次明确的、人工执行
的准备步骤；录制器本身不会静默下载第三方包。全新 checkout 中若包未安装，
先经用户确认执行精确版本命令：

```powershell
$repo = (Resolve-Path .).Path
$cache = "$repo\target\npm-cache"
$codexCache = "$repo\target\npm-cache-t006"
$env:PATH = "D:\Program Files\nodejs;" + $env:PATH

npx.cmd --yes --cache $codexCache "@openai/codex@0.146.0" --version
npx.cmd --yes --cache $cache "@agentclientprotocol/claude-agent-acp@0.63.0" --version
npx.cmd --yes --ignore-scripts --cache $cache "opencode-ai@1.18.8" --version
```

哈希目录由 npm 计算，不是协议版本。用版本输出筛选出本次精确运行时：

```powershell
$codex = (Get-ChildItem "$codexCache\_npx\*\node_modules\@openai\codex-win32-x64\vendor\x86_64-pc-windows-msvc\bin\codex.exe" |
  Where-Object { (& $_.FullName --version) -eq "codex-cli 0.146.0" } |
  Select-Object -First 1).FullName

$claudeAcp = (Get-ChildItem "$cache\_npx\*\node_modules\.bin\claude-agent-acp.cmd" |
  Where-Object { (& $_.FullName --version) -eq "0.63.0" } |
  Select-Object -First 1).FullName

$opencode = (Get-ChildItem "$cache\_npx\*\node_modules\opencode-windows-x64-baseline\bin\opencode.exe" |
  Where-Object { (& $_.FullName --version) -eq "1.18.8" } |
  Select-Object -First 1).FullName
```

为避免 OpenCode 读取真实项目配置，本次把 XDG 状态也隔离在任务构建目录：

```powershell
$env:XDG_CONFIG_HOME = "$repo\target\opencode-xdg-t006\config"
$env:XDG_DATA_HOME = "$repo\target\opencode-xdg-t006\data"
$env:XDG_STATE_HOME = "$repo\target\opencode-xdg-t006\state"
$env:XDG_CACHE_HOME = "$repo\target\opencode-xdg-t006\cache"
New-Item -ItemType Directory -Force $env:XDG_CONFIG_HOME,$env:XDG_DATA_HOME,$env:XDG_STATE_HOME,$env:XDG_CACHE_HOME | Out-Null
```

OpenCode 还会自行向上寻找项目。仅设置
`GIT_CEILING_DIRECTORIES=tests/fixtures` 时，它仍把父级 OneKaleidoscope 识别为
项目；那次临时录制因返回父项目路径而被丢弃，没有提交。成功录制前在 toy
project 内临时执行 `git init`，让 OpenCode 的项目根明确停在 sandbox；录完
立即删除这个临时 `.git`，仓库中不提交嵌套 Git 元数据。

各 agent 的九个场景使用同一组名字：

```powershell
$scenarios = @(
  "simple-turn",
  "tool-call",
  "permission-approve",
  "permission-deny",
  "file-change",
  "cancel",
  "error",
  "session-load",
  "elicitation"
)

foreach ($scenario in $scenarios | Where-Object { $_ -ne "session-load" }) {
  cargo run -p kaleido-recorder -- codex $scenario --executable $codex --timeout-secs 120
}

$env:KALEIDO_NODE_EXECUTABLE = "D:\Program Files\nodejs\node.exe"
foreach ($scenario in $scenarios | Where-Object { $_ -ne "session-load" }) {
  cargo run -p kaleido-recorder -- acp $scenario --bundled-executable $claudeAcp --timeout-secs 120
}

git -C tests/fixtures/sandbox init
try {
  foreach ($scenario in $scenarios) {
    cargo run -p kaleido-recorder -- opencode $scenario --executable $opencode --timeout-secs 120
  }
}
finally {
  Remove-Item -LiteralPath "$repo\tests\fixtures\sandbox\.git" -Recurse -Force
}
```

现有 fixture 不会被覆盖；在含 `06-cancel.jsonl` 的 checkout 中复现该场景会
明确报“refusing to overwrite”。应在全新 checkout 或人工保留旧样本后运行，
不能让录制器静默覆盖审计基准。

`08-session-load` 还需要先由该 agent 自己的 CLI 在同一 sandbox 建立真实会话。
Codex 本次使用：

```powershell
Push-Location tests/fixtures/sandbox
$codexSeedEvents = @(
  & $codex exec --json --skip-git-repo-check "Reply with exactly KALEIDO SESSION LOAD SEED" |
    ForEach-Object { $_ | ConvertFrom-Json }
)
$codexSeedExit = $LASTEXITCODE
Pop-Location
$threadId = ($codexSeedEvents |
  Where-Object { $_.type -eq "thread.started" } |
  Select-Object -First 1).thread_id
if ($codexSeedExit -ne 0 -or [string]::IsNullOrWhiteSpace($threadId)) {
  throw "Codex seed did not complete with a structured thread id"
}
cargo run -p kaleido-recorder -- codex session-load --thread-id $threadId --executable $codex --timeout-secs 120
```

最终 seed 确实输出了 `thread.started` 和结构化 thread id，但随后先做 5 次
WebSocket 重试、fallback 到 HTTPS 再做 5 次重试，最终以 `turn.failed`、进程
exit 1 结束。**有 thread id 但 seed turn 失败仍不是可加载的成功 seed。** 因此
没有把该 id 传给录制器；随后以空 id 调用 session-load 时，录制器按预期在发
协议报文前拒绝。旧实现曾用 `thread/list` 猜旧会话，当前实现只接受成功 seed
输出的精确 `--thread-id`，不会把 `thread.started` 单独偷换成 session-load 成功。

Claude 本次用 npm SDK 自备、已经证明能复用现有用户登录上下文的原生二进制
建立 seed：

```powershell
Push-Location tests/fixtures/sandbox
$claudeSeedJson = & $bundledClaude -p "Reply with exactly KALEIDO SESSION LOAD SEED" --output-format json
$claudeSeedExit = $LASTEXITCODE
Pop-Location
$claudeSeed = $claudeSeedJson | ConvertFrom-Json
$sessionId = $claudeSeed.session_id
if ($claudeSeedExit -ne 0 -or [string]::IsNullOrWhiteSpace($sessionId)) {
  throw "Claude seed did not complete with a structured session id"
}
cargo run -p kaleido-recorder -- acp session-load --session-id $sessionId --bundled-executable $claudeAcp --timeout-secs 120
```

seed 在 120 s 内没有结束，本轮没有得到可传给 `--session-id` 的结构化 id，故
仍未录到。旧实现随后曾真实执行 ACP `session/list`，但没有返回此 sandbox 的
会话；录制器现已收紧为没有精确 seed id 就在发协议报文前失败，不会选第一个旧
session。T-004 曾把一个同名 `claude.exe` 误判为录制残留；负责人核实它其实是
自己的 Claude Code 会话。T-006 的清理逻辑已改为只检查本次 spawn 的 PID 家族，
不再要求关闭无关的 Claude GUI/CLI。

OpenCode 的 session-load 使用同一隔离 XDG 状态，并用临时嵌套 toy repository
约束项目根：

```powershell
git -C tests/fixtures/sandbox init
Push-Location tests/fixtures/sandbox
& $opencode run --pure --format json --title "KALEIDO SESSION LOAD SEED" '"Reply with exactly KALEIDO SESSION LOAD SEED"'
Pop-Location
cargo run -p kaleido-recorder -- opencode session-load --executable $opencode --timeout-secs 120
Remove-Item -LiteralPath "$repo\tests\fixtures\sandbox\.git" -Recurse -Force
```

这份已提交的 T-004 session-load seed 当时因选择 Google provider 且缺少
对应凭据而以 `ProviderAuthError` 退出，但 OpenCode 确实持久化了这次真实会话。
录制器随后依次执行 `GET /session`、
`GET /session/{id}`、`GET /session/{id}/message`，得到提交的
`opencode/08-session-load.jsonl`。录制器要求会话目录规范化后精确等于
sandbox 且标题精确等于 `KALEIDO SESSION LOAD SEED`；不会加载列表中任意会话。
该历史 fixture 的 provider 结论不能外推到 T-006：本轮八个最终补录实际使用的
是隔离配置中的 `openai/gpt-5.6`。

## ACP initialize 与客户端能力复测

T-006 已把 ACP 客户端能力从旧 fixture 第 1 行真实记录的全 `false` 改为实现
真实支持的值。最终重试发送的 initialize 请求为：

```json
{"id":1,"jsonrpc":"2.0","method":"initialize","params":{"clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":true},"clientInfo":{"name":"OneKaleidoscope fixture recorder","version":"0.1.0"},"protocolVersion":1}}
```

对应实现不是只改 capability bit：录制器处理 `fs/read_text_file`、
`fs/write_text_file`、`terminal/create`、`terminal/output`、
`terminal/wait_for_exit`、`terminal/kill` 与 `terminal/release`。文件访问必须
规范化到当前 sandbox；terminal 只接受任务卡允许的确定性 `cargo run` 形式，
不会借能力声明放宽安全边界。

0.63.0 adapter 接受该 initialize，协议版本仍为 1；其真实能力响应仍包括
`loadSession:true`、session list/resume/close 等能力，随后 `session/new`
也成功。补录的 simple-turn、tool-call、permission approve/deny、file-change
与 error 最终仍都在 `session/prompt` 后 120 秒没有下一条协议消息。由此只能
得出“全 false capability 不是唯一根因”；不能把 initialize/new 成功或能力
元数据冒充 tool call、权限请求或模型回复。保留的
`acp-claude/06-cancel.jsonl` 是 T-004 的真实历史报文，不能为了反映新客户端
实现而改写其第 1 行。

## 3 × 9 场景尝试结果

| 场景 | Codex 0.146.0 | Claude ACP 0.63.0 | OpenCode 1.18.8 |
|---|---|---|---|
| 01 simple turn | 未录到：真实 app-server 完成 initialize/thread/start/turn/start，但 turn=`failed`；只有 `userMessage` item 和 11 个 error-info 通知，没有 agent text | 未录到：用 `fs.readTextFile=true`、`fs.writeTextFile=true`、`terminal=true` initialize，`session/new` 成功，`session/prompt` 后 120 秒无下一条协议消息 | 未录到：23 个真实事件，只有 session busy 与 assistant metadata；prompt POST 120 秒超时，0 text/reasoning/tool/command 证据 |
| 02 tool call | 未录到：turn=`failed`，只有 `userMessage`，没有 command/tool item 或 command exit | 未录到：真实能力 initialize/new 成功，prompt 后 120 秒超时，没有 `tool_call` update | 未录到：23 个真实事件，prompt POST 120 秒超时；0 tool/command 证据，故没有同一 call ID 的 start/progress/terminal |
| 03 permission approve | 未录到：turn 在任何 approval server request 前失败；0 command/tool/approval 证据 | 未录到：真实能力 initialize/new 成功，prompt 后 120 秒超时，没有 `session/request_permission`，未发送伪批准 | 未录到：23 个真实事件，prompt POST 120 秒超时；0 permission/tool/command 证据，未发送伪批准 |
| 04 permission deny | 未录到：turn 在任何 approval server request 前失败；0 command/tool/approval 证据 | 未录到：真实能力 initialize/new 成功，prompt 后 120 秒超时，没有可拒绝的 permission request | 未录到：23 个真实事件，prompt POST 120 秒超时；0 permission/tool/command 证据，未发送伪 reject |
| 05 file change | 未录到：turn=`failed`，没有 file-change item、diff update 或磁盘变化 | 未录到：真实能力 initialize/new 成功，prompt 后 120 秒超时，没有 tool call/diff；sandbox 已恢复 | 未录到：23 个真实事件，prompt POST 120 秒超时；0 tool/diff/command 证据，`editable.txt` 未改变且 restore 成功 |
| 06 cancel | 未录到：turn 在 commandExecution item 出现前失败，录制器明确拒绝把未发生的 cancel 算成功 | **已录到：`acp-claude/06-cancel.jsonl`**；真实 `session/cancel` 后 stopReason=`cancelled` | 未录到：23 个真实事件，prompt POST 120 秒超时；0 tool/command 证据，没有可中断 lifecycle |
| 07 error | 未录到：必败命令未启动，只有上游连接错误；不把网络错误偷换为命令错误 | 未录到：真实能力 initialize/new 成功，prompt 后 120 秒超时，没有 failed tool update | 未录到：23 个真实事件，prompt POST 120 秒超时；0 tool/command 证据，不把 provider 连接失败偷换为命令错误 |
| 08 session load | 未录到：seed 输出 `thread.started`，但 turn=`failed` 且 CLI exit 1；失败 seed 的 id 不可用，空 id 被录制器 fail-closed 拒绝 | 未补录：没有新的成功 Claude seed/session id；仍拒绝无精确 id 加载 | **已录到：`opencode/08-session-load.jsonl`**；T-004 真实 CLI seed虽以 provider auth 错误结束但已持久化，随后由 HTTP 列出、加载会话和消息 |
| 09 elicitation | 未录到：turn=`failed`，没有 `mcpServer/elicitation/request` | 不适用且未伪录：ADR-0007 已确认钉定 ACP v1.18 schema 不含 elicitation 方法 | 未录到：23 个真实事件，prompt POST 120 秒超时；0 question/permission/tool/command 证据 |

OpenCode 八个非 session-load 场景的最终重试各收到 23 个真实事件，统一只观察到
session busy 与 assistant metadata；每个 prompt POST 都在 120 秒超时，并且权限、
question、tool、diff、command 证据均为 0。这证明进程、HTTP 与 SSE 都实际跑通，
但不满足任何场景完成条件。八次的本次 spawn 进程家族清理和 sandbox restore
均成功；失败临时 transcript 仍被丢弃，不能引用为契约 fixture。实际 provider/model
是隔离配置中的 `openai/gpt-5.6`，不是负责人交互环境中已配置但本进程无法读取的
DeepSeek v4。

## Schema 与泄漏校验结果

基线校验会递归检查三个 agent 目录；fixture 根或未知目录中的 `.jsonl`、以及
symlink/junction fixture 都会直接失败，不能借目录位置逃过 schema/泄漏门禁。
对两份成功 fixture 的 14 条记录逐方法配对并校验通过：

```text
==> fixtures-verify
<== fixtures-verify: ok; 2 file(s), 14 record(s) (codex: 0, acp-claude: 1, opencode: 1)
```

本次没有 schema 校验失败条目；特别是
`session/update` 的 `available_commands_update` 摘要保留 ACP v1.18.0 要求的
`name`、`description`，长度证据放入协议保留的 `_meta`，因此仍通过 snapshot。
校验器同时要求 Codex/ACP request 在文件结束前有同方向 ID 空间中的匹配
response，并要求 OpenCode HTTP request/response 按 method + path 顺序闭合。

按 DoD 在真实 fixture 末尾临时插入一条 schema 合法、但
`sessionId="sk-test123"` 的 `session/cancel` 后，校验器按预期变红：

```text
xtask: fixture verification found 1 issue(s):
  tests/fixtures/acp-claude/06-cancel.jsonl:9: leak: secret prefix sk- at /payload/params/sessionId
```

随后删除该临时行；修改前后 SHA-256 均为
`B187EEAB9CE9D6A9FFE4D08045FD0AC079FDDC33CB93665969706BC14A972188`，
当前文件已恢复为 8 行。两份当前 fixture 的 SHA-256 为：

```text
acp-claude/06-cancel.jsonl
B187EEAB9CE9D6A9FFE4D08045FD0AC079FDDC33CB93665969706BC14A972188

opencode/08-session-load.jsonl
9F823AA5E4177A1B952FC2FB9022F230B0DB00B5A0DA6C9463683F134B8E6F58
```

## 12 个 UACP 事件变体 × 3 家覆盖

下表只把已提交 fixture 作为“已录到”。失败临时 transcript 即便曾出现某个
通用生命周期信号，也不进入可复用的契约覆盖。当前覆盖仍为 **2/36**，
其余 **34/36** 均逐格保留未录到原因。

| UACP 变体 | Codex | Claude Code / ACP | OpenCode |
|---|---|---|---|
| `MessageChunk` | 未录到（0.146.0 turn 失败，只有 user message） | 未录到（真实能力 initialize 后 prompt 120 秒超时） | 未录到（23 个事件中 0 text evidence） |
| `ThoughtChunk` | 未录到（0 reasoning item/delta） | 未录到（0 thought chunk） | 未录到（23 个事件中 0 reasoning evidence） |
| `ToolCallStart` | 未录到（0 command/tool item） | 未录到（0 tool-call update） | 未录到（23 个事件中 0 tool/command evidence） |
| `ToolCallUpdate` | 未录到（0 command/tool item） | 未录到（0 tool-call update） | 未录到（23 个事件中 0 tool/command evidence） |
| `ToolCallEnd` | 未录到（0 command/tool item） | 未录到（0 terminal tool-call update） | 未录到（23 个事件中 0 tool/command evidence） |
| `PermissionRequest` | 未录到（0 approval server request） | 未录到（0 `session/request_permission`） | 未录到（23 个事件中 0 permission evidence） |
| `Elicitation` | 未录到（0 `mcpServer/elicitation/request`） | 未录到且不适用（ADR-0007：钉定 schema 不含该方法） | 未录到（23 个事件中 0 question evidence） |
| `PlanUpdate` | 未录到（0 plan update） | 未录到（保留 fixture line 6 是命令能力清单，不是 plan） | 未录到（23 个事件中只有 busy 与 assistant metadata） |
| `DiffProduced` | 未录到（0 diff update） | 未录到（0 文件工具调用） | 未录到（23 个事件中 0 diff evidence） |
| `TurnStart` | 未录到（失败 transcript 未提交） | **已录到（`acp-claude/06-cancel.jsonl:5`）** | 未录到（失败 transcript 未提交） |
| `TurnEnd` | 未录到（失败 transcript 未提交） | **已录到（`acp-claude/06-cancel.jsonl:8`）** | 未录到（失败 transcript 未提交） |
| `Error` | 未录到（只有网络错误，没有目标命令错误） | 未录到（0 failed tool update） | 未录到（0 command evidence；provider 连接失败不是目标命令错误） |

## Elicitation 触发分析与 schema 摘录

### Codex

尝试方法：初始化时声明
`capabilities.mcpServerOpenaiFormElicitation=true`，并明确要求已配置且支持
structured elicitation 的 MCP server 发出一字段表单。实际 turn 在 MCP 调用前
因 response stream 断开而失败。

`schemas/codex/McpServerElicitationRequestParams.json` 的请求方法是
`mcpServer/elicitation/request`。params 的共同字段要求 `serverName`、
`threadId`，另有三种形状：

```json
{"required":["message","mode","requestedSchema"],"mode":{"enum":["form"]},"requestedSchema":{"$ref":"#/definitions/McpElicitationSchema"}}
{"required":["message","mode","requestedSchema"],"mode":{"enum":["openai/form"]},"requestedSchema":true}
{"required":["elicitationId","message","mode","url"],"mode":{"enum":["url"]},"url":{"type":"string"}}
```

只有实际配置的 MCP server 在运行中调用 MCP `elicitation/create` 才会产生这类
server-to-client request；一句普通模型提示不能保证触发。

### Claude ACP

执行：

```powershell
rg -n -i "elicitation/create|elicitation" schemas/acp/schema.json schemas/acp/meta.json
```

对钉定的 schema v1.18.0 无任何命中，因此没有可摘录的请求定义。该 schema
定义了 `session/request_permission`，但没有 ADR-0004 P-5 所期待的
`elicitation/create`。录制器在发 wire 报文前返回明确的 unsupported 结果，
没有手工发明方法名。

### OpenCode

尝试方法：让 question tool 向用户询问 Red / Green / Blue 三选一。T-006 最终
尝试收到 23 个真实事件，但只观察到 session busy 与 assistant metadata；
prompt POST 在 120 秒超时，question/permission/tool/diff/command 证据均为 0。

`schemas/opencode/openapi.json` 同时定义旧版与 v2 事件。旧版核心形状为：

```json
{
  "type": "object",
  "required": ["id", "type", "properties"],
  "properties": {
    "type": {"type": "string", "enum": ["question.asked"]},
    "properties": {
      "type": "object",
      "required": ["id", "sessionID", "questions"]
    }
  }
}
```

v2 的同构事件把枚举改为 `question.v2.asked`，问题数组引用
`QuestionV2Info`。上游只有在模型真正调用 question tool 时才发该事件；本次
隔离配置实际选中 `openai/gpt-5.6`，但 provider API 在受限进程中不可达，
prompt 没有进入 question tool。

## 权限审批：实际证据与 schema 预期

### 实际录制结论（R-8）

三家的 approve / deny 场景都已再次实际执行，但三家仍均未产生一次权限请求：

- Codex 0.146.0 完成 initialize/thread/turn 建立后，turn 在任何 command item
  或 approval server request 前以网络错误失败。
- Claude ACP 0.63.0 接受声明真实 fs/terminal 能力的 initialize，`session/new`
  也成功，但两个场景均在 prompt 后 120 秒超时，没有
  `session/request_permission`。
- OpenCode 1.18.8 的两个场景各收到 23 个真实事件，只有 session busy 与
  assistant metadata；prompt POST 120 秒超时，permission/tool/command 证据均为 0。

**因此本次仍没有三份可并排比较的“实际权限报文形状”，R-8 没有被一手
fixture 缓解，T-006 的核心补录目标也没有完成。** 不能把失败尝试、schema、
能力元数据或录制器测试里的合成消息冒充实际差异。

`opencode/08-session-load.jsonl:2` 与 `:4` 真实返回了会话持久化的
`permission` 策略数组，三项分别是 `question`、`plan_enter`、`plan_exit`，
形状均为 `{"permission":...,"pattern":"*","action":"deny"}`。这是会话配置，
不是运行时 `permission.asked` 请求，也没有 approve/deny response，因此不把
它冒充权限审批实录。

`acp-claude/06-cancel.jsonl:4` 真实返回了会话级 `modes` 与 `configOptions`：
当前模式为 `default`，候选包括 `auto`、`acceptEdits`、`plan`、`dontAsk`、
`bypassPermissions` 等。这同样只是权限能力/策略元数据，不是
`session/request_permission` 的实际请求形状。

### 仅用于后续复录定位的 schema 预期（不是实际报文）

- Codex schema 有分开的
  `item/commandExecution/requestApproval`、
  `item/fileChange/requestApproval` 和
  `item/permissions/requestApproval` server request；客户端响应使用固定 decision
  字符串或 permissions 结果。
- ACP schema 的 `session/request_permission` params 含 `sessionId`、
  `toolCall`、动态 `options[]`；每个 option 必须有 `optionId`、`name`、`kind`。
  返回 `outcome:"selected"` 加精确的 `optionId`，或 cancelled outcome。
- OpenCode schema 有 `permission.asked`（permission / patterns / metadata /
  always）和 `permission.v2.asked`（action / resources / save / source）两代事件；
  `POST /permission/{requestID}/reply` 的 body 是
  `{"reply":"once"|"always"|"reject"}`。

这三条只能指导下一次触发和对照，不能作为 PROTOCOL.md 的实录证据。

## R-11：GUI 登录态能否被 npm 自备二进制复用

结论：**能在技术上直接复用。**

验证时未设置 `ANTHROPIC_API_KEY`、`ANTHROPIC_AUTH_TOKEN`、
`CLAUDE_CODE_OAUTH_TOKEN` 或 Bedrock / Vertex / Foundry 凭据变量。对
`@agentclientprotocol/claude-agent-acp@0.63.0` 依赖自带的原生
`claude.exe` 运行：

```powershell
$bundledClaude = (Get-ChildItem "$cache\_npx\*\node_modules\@anthropic-ai\claude-agent-sdk-win32-x64\claude.exe" |
  Select-Object -First 1).FullName
& $bundledClaude auth status --json |
  ConvertFrom-Json |
  Select-Object loggedIn,authMethod,subscriptionType
```

安全字段结果为：

```text
loggedIn authMethod subscriptionType
-------- ---------- ----------------
True     claude.ai  pro
```

这证明 npm 自备二进制能复用 GUI/用户现有的认证上下文，无需额外登录，回答了
R-11 的开箱体验问题。`auth status` 本身不能证明凭据字节一定来自
`~/.claude` 文件而不是操作系统 credential store，因此不对存储介质作额外
推断。另用 `claude.exe -p` 做了真实模型 seed 调用，但 120 s 内没有结束，也
没有建立可由 ACP 列出的 sandbox 会话。因此本机证据把“登录态可复用”与
“当前隔离进程能完成模型调用”区分开：前者已证实，后者未证实。

## 已发现的上游/环境问题

1. 精确 Codex 0.146.0 在模型响应阶段因当前进程的 socket/HTTP 网络访问被拒而
   失败；app-server initialize 和 thread/turn 生命周期本身可用。CLI seed
   虽发出 `thread.started`，但 `turn.failed` 且 exit 1，不能用作 session-load。
2. OpenCode 全局 CLI 确实安装在 nvm 位置，但本进程对该目录返回
   `PermissionDenied`；任务目录内的精确 1.18.8 原生包可运行。隔离 XDG 状态
   避免读取真实配置，也意味着本次实际落到 `openai/gpt-5.6`，而没有使用负责人
   交互环境已配置的 DeepSeek v4。这是本轮明确偏离，不能推断负责人“未登录”。
3. ACP schema v1.18.0 没有 elicitation 定义；ADR-0007 已据此把 ACP elicitation
   排除出本轮强制补录范围，录制器继续 fail-closed，不发明 wire 方法。
4. `claude-agent-acp@0.63.0` 当前 package.json 实际声明
   `@agentclientprotocol/sdk: 1.3.0`、`@anthropic-ai/claude-agent-sdk: 0.3.220`；
   ADR-0004 记录的是 `0.25.0` 与 `0.3.169`。本卡未修改 ADR 或 schema。
5. 技术上复用 claude.ai 登录态不等于第三方产品已获得分发/认证政策许可；
   产品化前仍需单独确认合规路径。
6. OpenCode 自己的项目发现没有遵守为子进程设置的
   `GIT_CEILING_DIRECTORIES`；必须用嵌套 toy repository 才停止在 sandbox。
   录制器同时新增目录与精确 seed 标题校验，避免误取列表中的其他会话。
7. Windows `Path::canonicalize()` 会给 sandbox 产生 `\\?\` verbatim 前缀，
   而 OpenCode HTTP 返回普通盘符路径；脱敏器已覆盖两种等价拼写。修复前新增
   回归测试确实把 `<SANDBOX>` 误成 `<OUTSIDE_PATH>` 并变红，修复后通过。
8. T-004 把一个同名 `claude.exe` 误判为录制残留；负责人确认它是自己的
   Claude Code 会话，而不是 recorder 子进程。T-006 已改为只检查本次 spawn
   根 PID 的进程家族，最终尝试清理成功；hostd/recorder 不能按可执行文件名要求
   用户关闭无关 GUI/CLI。
9. 本卡已在 Windows 上实测五层发现，并为非 Windows 实现裸可执行名与标准
   用户目录探测；尚未在 macOS / Linux 主机上执行 GUI 安装位置的 G8 冒烟。
   特别是 macOS GUI bundle 的已知位置仍需后续平台任务补充实测，不能从本次
   Windows 结果推断其可用。
10. 早期 ACP 重试暴露了进程家族清理问题；修正后最终 ACP 与 OpenCode 尝试都
    完成本次 spawn 家族清理和 sandbox restore。动态 `npm prefix --global`
    探测仍保持禁用，避免发现步骤额外启动 npm/cmd 子进程；已知 npm prefix
    继续通过确定性默认位置、环境变量和 `.npmrc` 静态读取。
11. 任务卡写“当前用户名出现即失败”，同时又要求 payload 键原样保留。Linux
    CI 的常见用户名 `root` 会与 OpenCode 真实结构字段名 `root` 冲突：若扫描
    JSON key，则无法既保留真实字段又通过门禁。当前实现因此只对 JSON value 和
    原始文本值扫描用户名，不把同名 key 当作泄漏；用户名、家目录、凭据和路径的
    值仍严格失败；路径样式 key 仍检查越界。主管已在 T-006 裁决该行为正确。
12. 把 ACP client capabilities 从全 `false` 改为真实 fs/terminal 能力并实现
    对应方法后，0.63.0 仍在 prompt 后 120 秒无消息；能力协商假设不足以解释
    ACP 整列缺失，仍需在网络/adapter/Claude 调用链继续诊断。
13. Codex 0.144.6 → 0.146.0 的语义 diff 一次产生 2,380 条 Codex 路径变化，
    是 R-4 的实际高漂移证据；本轮已 refresh 到 0.146.0，但后续仍必须在升级
    runtime 前先 diff，不能让新二进制对照旧 snapshot 录制。
