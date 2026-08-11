# OneKaleidoscope

[English](README.md) | [简体中文](README_ZH.md)

离开电脑后，继续通过手机掌控 PC 上运行的 AI 编码 Agent。

> [!WARNING]
> OneKaleidoscope 正在持续迭代，**目前并不是完整产品**。仓库现阶段提供的是经过验证的 Rust 本地纵切和协议实现，还没有可安装的 Android/iOS App，也没有可用于生产的远程服务。迭代期间 API、存储格式和行为都可能变化。

OneKaleidoscope 是 AI 编码 Agent 的控制平面，不是终端镜像。PC 侧 Session Broker 通过各 Provider 的公开结构化协议连接会话，把各家报文归约为规范化状态，再向移动端提供读模型和命令。目标产品将支持 Codex、Claude Code 和 OpenCode，包括持久化会话、人工审批、输入队列、断线恢复和跨 Agent 工作流。

Agent 数据绝不通过抓取终端、解析 ANSI/TUI、截图、OCR，或轮询 transcript 后冒充实时状态来获取。

## 当前状态

R0（文档基线）、R1（规范化合同）和 R2（单 Provider 本地纵切）已经完成。项目正在收尾 R3 前置协议工作；R3 将交付第一个 Android 局域网纵切。进度以 [docs/STATUS.md](docs/STATUS.md) 为唯一真源，实施顺序见 [docs/MILESTONES.md](docs/MILESTONES.md)。

仓库中已有实现和证据支持的部分：

- 规范化协议 v0.1，覆盖 Session、Turn、Item、命令、Attention、队列、能力、投影、错误和工作流。
- Provider 中立的 runtime session 抽象。
- Codex app-server 适配器：钉定 JSON Pointer 解码、reducer 和 stdio JSON-RPC 进程传输。
- Codex 真实或录制报文到 canonical state、追加式 durable log，再到六个读模型的本地端到端路径：会话索引、对话记录、实时活动、输入队列、待处理事项和 runtime 能力。
- 支持真实运行、fixture 重放、重启恢复和投影查看的诊断命令。
- 敏感正文的内容寻址存储、本地命令幂等，以及日志/脱敏检查。
- Codex app-server 连续版本范围 `0.146.0`–`0.147.0` 的兼容性证据。
- Kotlin 与 Swift 已编译通过 UniFFI API 形状探针，覆盖 callback、object、async 和 throwing；这些只是绑定探针，不是移动端 App 实现。
- Windows、macOS、Linux Rust CI，以及 Kotlin、Swift 消费端编译门禁。

尚未完全实现的部分：

- Android 和 iOS App。
- 局域网/互联网传输、配对、端到端加密、P2P 和密文 relay。
- Claude Code、OpenCode 的产品级适配器，以及 ACP 兼容路径。
- 跨 Agent 工作流调度，以及规划中的 Claude → Codex → Claude 工作流。
- 所有原生 CLI/GUI 表面的实时附着验证。
- 真正送达移动端的 steer、完整 live-control 语义、文件/代码浏览、Git 操作、打包和发布硬化。

部分上游原生 GUI/CLI 目前没有稳定、公开的第三方实时附着合同。这些缺口会保持显式记录；项目不会用终端抓取替代，也不会把“能读历史”描述成“能实时控制”。

## 架构

```text
Codex / Claude Code / OpenCode 的公开结构化协议
                              │
                              ▼
                       PC Session Broker
           decoder → reducer → canonical state → durable log
                              │
                        投影与命令
                              │
                   LAN / P2P / 加密 relay          （规划中）
                              │
                              ▼
                       Android / iOS                （规划中）
```

核心规则：

- 历史访问与实时 runtime 控制是两种独立能力。
- Provider 报文必须先归约为 canonical state，再进入 UI 投影。
- 状态必须能由快照与对应 cursor 之后的 durable log 重建。
- 能力属于具体 runtime 连接；客户端不能按 Provider 名称分支。
- 服务器只负责协调和转发密文，不持有 Provider 凭据、项目文件或业务明文。

完整模型见 [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)，规范化合同见 [docs/PROTOCOL.md](docs/PROTOCOL.md)。

## 仓库结构

| 路径 | 用途 |
|---|---|
| `crates/kaleido-proto` | 规范化类型、命令、校验器和可导出到 UniFFI 的合同 |
| `crates/kaleido-state` | canonical state、durable log、内容存储、命令处理和已实现的六个投影 |
| `crates/kaleido-adapter` | Provider 中立的 runtime trait、身份、内容访问和能力证据 |
| `crates/kaleido-adapter-codex` | Codex app-server decoder、reducer、runtime 进程传输和漂移守卫 |
| `crates/kaleido-hostd` | 组合根和当前的 `slice run/replay/show` 诊断 CLI |
| `crates/kaleido-core` | 最小 UniFFI 门面及 Kotlin/Swift 消费探针；目前还不是移动端产品 runtime |
| `schemas` | 字节级保真的上游 schema 快照、required-surface 归属和漂移历史 |
| `tests/fixtures` | 合同测试和 reducer 测试使用的真实结构化协议录制证据 |
| `xtask` | 本地 CI、依赖、fixture 和 schema 工具 |
| `spikes` | 已冻结的研究资产，不代表当前产品架构 |

## 快速开始

安装 [Rustup](https://rustup.rs/)，并在仓库根目录执行命令。`rust-toolchain.toml` 会选择 Rust 1.94.0 及所需组件。

构建整个 workspace：

```text
cargo build --workspace --locked
```

把仓库中真实录制的 Codex fixture 重放到一个全新的诊断日志目录，然后查看全部已实现投影：

```text
cargo run -p kaleido-hostd -- slice replay --fixture tests/fixtures/codex/01-simple-turn.jsonl --log-dir target/kaleido-demo
cargo run -p kaleido-hostd -- slice show --log-dir target/kaleido-demo --projection all
```

当前实时诊断路径可以在给定原生 Codex 可执行文件和项目目录后启动 app-server 会话：

```text
cargo run -p kaleido-hostd -- slice run --executable <Codex可执行文件路径> --project-root <项目目录> --log-dir target/kaleido-live --prompt "检查这个项目并给出摘要"
```

这是开发阶段的诊断接口，不是稳定的用户 CLI。它能够提交 prompt，并通过可选参数处理第一条受支持的文件变更审批。请求的 steer 会明确保留在 Broker 队列中，除非已经观察到 runtime 的送达证据；“已排队”不能展示成“已送达控制”。

## 开发

唯一的本地门禁入口是：

```text
cargo xtask ci
```

它依次检查格式、依赖边界、禁用模式、Clippy 零告警、workspace 测试和 fixture 完整性。Schema 漂移检查会调用本机安装的上游工具并访问网络，因此单独执行：

```text
cargo xtask schema diff
```

修改项目前，请先阅读 [CLAUDE.md](CLAUDE.md)、[AGENTS.md](AGENTS.md) 和 [docs/STATUS.md](docs/STATUS.md) 中当前的下一步。协议改动必须在同一次变更中同步协议文本、规范化 Rust 类型、依赖代码、测试和 ADR。每个功能测试都要包含拒绝或错误路径，并保证断言在实现被破坏时真的会失败。

常用文档：

- [需求](docs/REQUIREMENTS.md)
- [当前状态](docs/STATUS.md)
- [架构](docs/ARCHITECTURE.md)
- [协议](docs/PROTOCOL.md)
- [里程碑](docs/MILESTONES.md)
- [开发指南](docs/DEVELOPMENT.md)
- [上游兼容性](docs/UPSTREAM.md)

## 许可证

本仓库目前标记为 `UNLICENSED`，未提供开源许可证。
