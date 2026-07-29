# M1 执行队列 —— 依次粘贴给 Codex

> 里程碑：**M1 地基与实测材料**（见 [MILESTONES.md](../MILESTONES.md)）
> 用法：按顺序执行。**每一条等 Codex 交付完再发下一条**，因为后一张卡依赖前一张的产物。
> 全部完成后整批交回主管审核。

## 当前执行顺序（2026-07-28 二次修订）

T-003 阻塞 → 拆出 T-005；T-004 阻塞（Windows 可执行文件解析）→ 顺序调整为：

| # | 卡 | 状态 |
|---|---|---|
| 1 | T-001 iroh spike | ✅ 已交付（待审） |
| 2 | T-002 工程守卫 | ✅ 已交付（待审） |
| 3 | T-003 schema 快照（修订 R1） | ✅ 已交付（待审） |
| 4 | **T-004（修订 R2）** 真实录制 | ⏩ **下一条发这个** |
| 5 | **T-005**（生成链落地，第 0~4 步一次做完） | 待 T-004 |

**三次改序说明**：

- T-003 阻塞 → 拆出 T-005
- T-004 阻塞（报 `claude` 不可发现）→ 曾计划先做 T-005 阶段一顶上
- **负责人说明安装形态 + 核实到 Claude Code SDK 自带二进制（[ADR-0006](../adr/0006-agent-discovery.md)）→ T-004 阻塞解除，恢复原顺序**

因此 T-005 不再需要分阶段，一次做完即可（fixture 往返验证需要 T-004 的产物）。
下方保留了 T-005 分阶段的 prompt，**仅作 T-004 再次阻塞时的备选**。

**共同前提**：Codex 每次动手前都必须重读 `AGENTS.md`。四条 prompt 里都写了，不要删。

---

## Prompt 1 / 4 —— T-001 iroh 打洞 spike

```
你是 OneKaleidoscope 项目的实现方（Implementer）。架构与协议由项目主管定义，
你负责把任务卡变成可运行、可验证的代码。你的交付会被逐条审核。

动手前必读（按顺序，全文，不要跳读）：
1. AGENTS.md —— 你的行为规范，尤其 §2 铁律、§3 编码规范、§4 交付格式、§5 阻塞报告格式
2. docs/tasks/T-001.md —— 本次任务卡，其 Definition of Done 是唯一验收标准
3. docs/REQUIREMENTS.md §6.1、§8 门禁 G0、§9 风险 R-1
4. docs/adr/0001-technology-selection.md C-1（iroh 1.0 的类型改名）

本次任务：执行 docs/tasks/T-001.md。

三条最容易翻车的地方：

1. iroh 必须是 1.0.x，不是 0.x。
   训练数据里绝大多数 iroh 示例是 0.x 的。1.0 已把 NodeId 改成 EndpointId、
   NodeAddr 改成 EndpointAddr、删掉 conn_type()、把 mDNS 拆到独立 crate。
   动手前先访问 https://docs.rs/iroh/latest/iroh/ 核对真实 API。
   任务卡「硬性约束」里列了我已核对过的一批签名，可以直接用。
   若实际 API 与任务卡不符：停下来按 AGENTS.md §5 报告，不许自己发明等价物，
   不许退化成轮询猜测连接类型。

2. 测量口径不许放宽。以下一律视为交付失败：
   把 relay 连接算成成功、观测窗口缩短到打洞来不及完成、只测 LAN 就交差、
   连接失败时不写记录（失败样本也是 G0 的数据）。

3. 测试必须能失败。写完每个测试，把实现改坏一次确认它会变红。
   任务卡点名要求的边界值测试（恰好 60.0% 判为 OPTIONAL）和 3 条错误路径测试，
   一条都不能少。不许用 #[ignore] 绕过。

交付时按 AGENTS.md §4.2 附带：DoD 逐条勾选、cargo test 全文输出、
Windows 上 listen+dial 两侧完整 stdout 与 results.jsonl 全文、summarize 输出、
aarch64-linux-android 交叉编译结果、新增依赖及理由、偏离说明、发现的问题。

边界：任务卡「边界」一节列出的文件一律不许碰。本任务不创建 crates/ 下的任何东西。

现在开始。
```

---

## Prompt 2 / 4 —— T-002 Workspace 骨架与工程守卫

```
继续 OneKaleidoscope 项目，你仍是实现方（Implementer）。

动手前必读：
1. AGENTS.md —— 全文重读一遍，尤其 §2.4（不许留半成品）、§3.1（Rust 规范）
2. docs/tasks/T-002.md —— 本次任务卡
3. docs/REQUIREMENTS.md §2 端 A 的平台差异清单

本次任务：执行 docs/tasks/T-002.md。

这张卡的核心价值是把 AGENTS.md 里「靠自觉」的规范变成机器强制。所以：

1. 光把 lint 写进配置不算完成。DoD 明确要求你故意写一行 unwrap()，
   跑 clippy 让它变红，把那次失败输出粘出来，然后删掉。
   没有这份证据的交付一律打回 —— 配置写了但没生效是最常见的假完成。

2. lint-forbidden 的禁用模式没有豁免机制。如果你认为某处必须豁免，
   按 AGENTS.md §5 报告，不要自己加 allow-list。

3. 不要引入 just / make / .ps1 / .sh 脚本，也不要引入 cargo-deny 等额外工具链。
   xtask 用 Rust 写，三平台行为一致。

4. T-001 的 spike 代码在新 lint 下必须依然全绿。有冲突就修 spike，不许放宽 lint，
   修改内容要在交付说明里讲清楚。

5. 本仓库暂无远端，CI 无法实际运行。DoD 只要求 workflow 是合法 YAML、
   与本地 cargo xtask ci 等价、且在 Windows 上本地跑通。不要为此编造 CI 通过的结论。

交付时按 AGENTS.md §4.2 附带全部要求项。

边界：任务卡「边界」一节列出的文件一律不许碰。本任务不创建 crates/ 下的任何东西。

现在开始。
```

---

## Prompt 3 / 5 —— T-003（修订版 R1）schema 快照与漂移监控

> **本条已按 T-003 阻塞报告重写。** 原卡的生成器部分已移出为 T-005（Prompt 5）。
> 如果 Codex 之前已经收到过旧版 Prompt 3，请直接发这条新的。

```
继续 OneKaleidoscope 项目，你仍是实现方（Implementer）。

你上一轮就 T-003 提交的阻塞报告已被主管受理，诊断被独立核实为正确：
progenitor 官方只支持 OpenAPI 3.0.x 而 OpenCode 输出 3.1.0；typify 无法解析
Codex schema 的嵌套 definitions。你没改 schema、没手写类型、没动仓库，处置正确。

主管据此签发了 docs/adr/0005-schema-normalization-layer.md：
- 「类型不许手写」的铁律保留
- 允许在 schema 与生成器之间插入受纪律约束的规范化层
- typify / progenitor 从「必须使用」降级为「首选候选」
- 生成链部分从 T-003 移出，独立为 T-005，顺延到 T-004 之后执行

动手前必读：
1. docs/adr/0005-schema-normalization-layer.md —— 新签发，全文
2. AGENTS.md §3.2 —— 已按 ADR-0005 修订，重新读一遍
3. docs/tasks/T-003.md —— 已降范围，注意顶部的「修订 R1」说明
4. docs/adr/0004-acp-version-pinning.md —— ACP 的确切钉定值已写进去了，直接用

本次任务：执行修订后的 docs/tasks/T-003.md。

范围已经变小，现在只做三件事：抓快照、记版本、做语义 diff。

1. 本卡不做任何类型生成。如果你发现自己在装 typify 或 progenitor，说明走错卡了。

2. 版本号一律钉死确切值，不许写 latest 或 ^。schemas/VERSIONS.md 必须记录
   每份 schema 的确切版本号与逐字可复制的抓取命令 —— 别人照着敲要能复现出同一份文件。

3. ACP 的来源已经核实过并写进任务卡（commit 48b2abf...、schema/v1/schema.json）。
   取完请核对 commit hash 是否一致，不一致要报告。

4. diff 必须是语义化的（解析 JSON 后比较），键顺序变化不算漂移。
   DoD 要求你人为改一个字段验证它会被检出并指出 JSON 路径，把输出粘出来。

5. schemas/ 下的快照从此是只读基准，后续任何任务都不许改它 —— 它是判断
   「上游改了什么」vs「我们改了什么」的唯一凭证。

人类前置条件：本机需已安装并可运行 codex 与 opencode。缺失请立即报告阻塞，不要跳过该家。

交付时按 AGENTS.md §4.2 附带全部要求项。

边界：任务卡「边界」一节列出的文件一律不许碰。本任务不创建 crates/ 下的任何东西。

现在开始。
```

---

## Prompt 4（修订 R2）—— T-004 三家 agent 真实事件录制 ⏩ 现在发这条

> R1 判定阻塞根因为 Windows 解析（R-6）；R2 在负责人说明安装形态后，
> 进一步核实到 Claude Code 不需要 CLI，**`claude` 的阻塞彻底解除**。

```
继续 OneKaleidoscope 项目，你仍是实现方（Implementer）。

关于你上一轮就 T-004 提交的环境阻塞：主管核对后判定阻塞不成立，两个根因都已澄清。

【根因一：Windows 可执行文件解析，不是环境缺失】

你在 T-003 交付的 schemas/VERSIONS.md 里，自己记录了在同一环境下成功执行过：
    codex.cmd app-server generate-json-schema --out schemas/codex
    opencode.cmd serve --pure --hostname 127.0.0.1 --port 4096 --log-level ERROR

也就是说 codex 与 opencode 装好了、能跑，只是不能用裸名字启动。

Windows 上 npm 全局安装会生成三个文件：codex（无扩展名的 POSIX sh 脚本）、
codex.cmd、codex.ps1。直接 exec 那个无扩展名的 sh 脚本，Windows 返回的就是
"Access is denied"。「不可发现」是同一现象的另一面。
这正是 REQUIREMENTS §9 的 R-6 与 AGENTS.md §3.5 点名的陷阱。

【根因二：Claude Code 根本不需要 CLI】

负责人的实际安装形态是：Claude Code 只装了 GUI、Codex 装了 CLI+GUI、OpenCode 只装了 CLI。
并明确要求同时支持 GUI 与 CLI 形态的协议识别。

核实结果：@anthropic-ai/claude-agent-sdk 通过 npm optionalDependencies
自带各平台的 Claude Code 原生二进制。官方文档原文：
"The SDK bundles a native Claude Code binary for your platform as an optional
dependency… You don't need to install Claude Code separately."

所以 Claude Code 的录制方式是：用钉定版本的 @agentclientprotocol/claude-agent-acp
经 npm/npx 起 ACP 进程。不要去找用户的 claude 命令，它不存在也不需要存在。

主管据此签发了 docs/adr/0006-agent-discovery.md。

动手前必读：
1. docs/adr/0006-agent-discovery.md —— 新签发，全文
2. docs/tasks/T-004.md —— 注意新增的两节：「Windows 上的可执行文件解析」与「Agent 发现」
3. AGENTS.md §3.5（跨平台纪律）、§2.3（fixture 必须真实）
4. docs/REQUIREMENTS.md §4.5（12 个事件变体）、§6.3（安全红线）、§9 R-6 / R-11

本次任务：执行修订后的 docs/tasks/T-004.md。

五件事需要特别注意：

1. 先写平台感知的可执行文件解析：Windows 上按 PATHEXT 顺序查找（.cmd / .exe / .bat），
   命中 .cmd 时按 Windows 方式启动；非 Windows 用裸名字。
   逻辑收敛在 spikes/kaleido-recorder/src/platform/ 下，配单元测试。
   这段代码不是一次性的 —— hostd 将来 spawn 三家 agent 用的就是同一套逻辑。

2. 禁止「PATH 上没有 CLI ⇒ 该 agent 不可用」的推断。探测报告必须区分四种情况：
   没装 / 装了但解析方式不对 / 装了 GUI 没装 CLI（多数情况下仍可用）/ 装了但未登录。

3. 必须回答一个问题（对应新增风险 R-11）：负责人的 Claude Code 是 GUI 登录的，
   GUI 写入 ~/.claude 的登录态能否被 npm 自备的二进制直接复用？
   能复用就说明验证方式；不能就说明用户还需要做什么。如实报告，这影响开箱体验。

4. 其余要求不变：不许编造报文、payload 原样保留、必须在 tests/fixtures/sandbox/ 里录、
   schema 校验失败要原样上报、12×3 覆盖度表如实填（空格子比假数据有价值）。

5. 若某家仍然录不成，不要整卡阻塞 —— 先把能录的录完，那一家在覆盖度表里标注原因。

交付时按 AGENTS.md §4.2 附带全部要求项，并单独整理一段
「三家在权限审批上的实际报文形状差异」（主管等着用，对应 R-8）。

边界：任务卡「边界」一节列出的文件一律不许碰。schemas/** 只读。

现在开始。
```

<details>
<summary>Prompt 4 原始版本（已被上面替代，仅存档）</summary>

```
继续 OneKaleidoscope 项目，你仍是实现方（Implementer）。

动手前必读：
1. AGENTS.md —— 全文重读，尤其 §2.3（契约测试必须用真实录制的 fixture，不许手工编造）
2. docs/tasks/T-004.md —— 本次任务卡
3. docs/REQUIREMENTS.md §4.5（12 个事件变体）、§6.3（安全红线）
4. docs/adr/0003-agent-attach-semantics.md、docs/adr/0004-acp-version-pinning.md

本次任务：执行 docs/tasks/T-004.md。

这张卡的产物是主管设计 PROTOCOL.md 的原始材料，所以它的诚实度比完整度重要：

1. 绝对不许编造报文。录不到的场景（尤其 09-elicitation）就如实写录不到，
   并交代你试了什么、上游在什么条件下才会发这个请求。
   假 fixture 会让整个协议设计建立在幻觉上，这是本项目最严重的违规。

2. tests/fixtures/README.md 里那张「12 个事件变体 × 3 家 agent」的覆盖度表，
   空格子比假数据有价值得多。如实填。

3. payload 必须原样保留 —— 不许重排键、不许美化、不许丢字段。
   脱敏是唯一允许的改动，且必须是确定性替换（同一原串在所有文件里替换成同一占位符）。

4. 录制必须在 tests/fixtures/sandbox/ 这个专用玩具项目里进行。
   绝不许在本仓库或任何真实项目里录 —— 会把代码内容写进 fixture，违反 REQUIREMENTS §6.3。

5. schema 校验失败不一定是你的错，也可能是上游 schema 与实际报文不一致。
   这种情况必须原样上报（哪个方法、哪个字段、schema 说什么、实际发什么）。
   不要为了让校验通过而修改 fixture。

6. 三家在权限审批上的实际报文形状差异，请单独整理一段报告出来。
   主管正等这份证据做协议设计（对应风险登记 R-8）。

人类前置条件：本机需已安装并登录 codex、claude、opencode。录制会消耗 API 额度。
缺失请立即报告阻塞。

交付时按 AGENTS.md §4.2 附带全部要求项。

边界：任务卡「边界」一节列出的文件一律不许碰。schemas/** 只读。
本任务不创建 crates/ 下的任何东西。

现在开始。
```

</details>

---

## Prompt 5 · 阶段一 —— T-005 生成链评估（第 0~3 步）

> **备选路径**：仅在 T-004 再次阻塞、需要让队列继续走时才发这条。
> T-004 正常交付的话，直接用下面的 Prompt 5 合并版（存档折叠里那份）一次做完。

```
继续 OneKaleidoscope 项目，你仍是实现方（Implementer）。

T-004 因 Windows 可执行文件解析问题暂缓，主管调整了执行顺序：先做 T-005 的前三步。
这三步只需要 T-003 已交付的 schemas/，不需要三家 agent 跑起来，所以现在就能做。

这张卡直接来自你在 T-003 提交的阻塞报告。主管受理了，并签发了 ADR-0005。

动手前必读：
1. docs/adr/0005-schema-normalization-layer.md —— 全文。本卡的全部纪律都在里面
2. AGENTS.md §3.2 —— 已按 ADR-0005 修订
3. docs/tasks/T-005.md —— 注意顶部「修订 R1」的阶段划分

本次任务：执行 docs/tasks/T-005.md 的第 0 步到第 3 步。
第 4 步（fixture 往返验证）属于阶段二，等 T-004 交付后再回来做 ——
相关 DoD 条目标注「待阶段二」即可，不算未完成。

但请注意：阶段一不得下最终结论。工具选型的最终拍板必须等第 4 步的证据。
证据表里该列先留空，不要用「能编译」代替「能用」。

五件事需要特别注意：

1. 第 0 步先补证据，不要跳过。
   你上次说「单独生成官方 ServerRequest.json 同样触发 typify 的未实现分支」，
   但没给出那个分支的具体错误。先补上完整 panic 消息、RUST_BACKTRACE=1 的关键帧、
   以及触发它的 schema 片段。这条决定后续走向：如果 typify 只是不认嵌套 definitions，
   规范化层能解决；如果它对 ServerRequest 这类结构本身就不支持，规则写再多也没用。

2. OpenCode 两条路线都要评估，不许只试一条。
   路线 A 是 3.1→3.0 降级 + progenitor；路线 B 是换用原生支持 3.1 的生成器
   （已知候选 openapi-to-rust，注意它 pre-1.0、star 数低、官方声明生成 API 可能变）。
   两条都跑到能下结论为止，证据填进 docs/gates/T-005-evidence.md。
   另外请明确回答：progenitor 不处理 SSE，而 /event SSE 流是 OpenCode 的主数据通路，
   走路线 A 的话 SSE 侧的类型从哪来？

3. 规范化规则的纪律是硬的：纯机械变换，禁止删字段、放宽约束、猜测语义。
   每条规则要有名字、单元测试、before/after 断言。规则数超过 10 条就停下来报告，
   那说明这条生成链不健康，该换工具而不是继续堆规则。

4. schemas/ 是只读基准，一个字节都不许改。规范化产物写进 target/ 或已 gitignore
   的目录，不提交。

5. 结论是「做不到」也是合格交付，只要证据充分。
   不合格的是：为了打勾而放宽类型、删定义、把生成不了的东西悄悄移出子集。

交付时按 AGENTS.md §4.2 附带全部要求项。

边界：任务卡「边界」一节列出的文件一律不许碰。

现在开始。
```

---

## Prompt 5 · 阶段二 —— T-005 fixture 往返验证（第 4 步）

> **T-004 交付之后再发。**

```
继续 OneKaleidoscope 项目，你仍是实现方（Implementer）。这是 M1 的最后一步。

T-004 已交付，tests/fixtures/ 下现在有真实录制的报文。回来把 T-005 的第 4 步做完。

动手前必读：
1. docs/tasks/T-005.md 第 4 步与 DoD
2. tests/fixtures/README.md —— T-004 的覆盖度表
3. docs/gates/T-005-evidence.md —— 你在阶段一填的证据表

本次任务：执行 T-005 第 4 步，并据此给出最终选型结论。

这一步是本卡最有价值的部分：
用生成的类型反序列化 T-004 的真实报文，再序列化回去，与原始 payload 做语义比较
（键顺序无关）。能编译不代表能用 —— 字段可选性错了、枚举变体缺了、untagged 用错了，
只有真实报文能暴露。

要求：
1. 失败条目逐条列出：哪个文件哪一行、哪个类型、差异在哪
2. 不要求 100% 通过，要求诚实。不许改成 serde_json::Value 蒙混过关，
   那等于放弃类型安全
3. 把往返通过率填进证据表，然后给出最终建议 —— 直说你倾向哪条路线以及为什么
4. 若结论是某家无法自动生成，说明你建议的替代方案与其漂移检测手段

交付时按 AGENTS.md §4.2 附带全部要求项，并声明规范化规则中没有删字段、
放宽约束、猜测语义的变换。

边界：schemas/** 与 tests/fixtures/** 都是只读。

现在开始。
```

<details>
<summary>Prompt 5 合并版原文（已被上面两阶段替代，仅存档）</summary>

```
继续 OneKaleidoscope 项目，你仍是实现方（Implementer）。这是 M1 的最后一张卡。

这张卡直接来自你在 T-003 提交的阻塞报告。主管受理了，并签发了 ADR-0005。
现在回来把这件事做完 —— 但规则变了，先读清楚再动手。

动手前必读：
1. docs/adr/0005-schema-normalization-layer.md —— 全文。本卡的全部纪律都在里面
2. AGENTS.md §3.2 —— 已按 ADR-0005 修订
3. docs/tasks/T-005.md —— 本次任务卡，按其「执行顺序」逐步做，不要跳步
4. tests/fixtures/README.md —— T-004 的覆盖度表，第 4 步要用

本次任务：执行 docs/tasks/T-005.md。

五件事需要你特别注意：

1. 第 0 步先补证据，不要跳过。
   你上次说「单独生成官方 ServerRequest.json 同样触发 typify 的未实现分支」，
   但没给出那个分支的具体错误。先补上完整 panic 消息、RUST_BACKTRACE=1 的关键帧、
   以及触发它的 schema 片段。这条决定后续走向：如果 typify 只是不认嵌套 definitions，
   规范化层能解决；如果它对 ServerRequest 这类结构本身就不支持，规则写再多也没用。

2. OpenCode 两条路线都要评估，不许只试一条。
   路线 A 是 3.1→3.0 降级 + progenitor；路线 B 是换用原生支持 3.1 的生成器
   （已知候选 openapi-to-rust，注意它 pre-1.0、star 数低、官方声明生成 API 可能变）。
   两条都跑到能下结论为止，证据填进 docs/gates/T-005-evidence.md。
   另外请明确回答：progenitor 不处理 SSE，而 /event SSE 流是 OpenCode 的主数据通路，
   走路线 A 的话 SSE 侧的类型从哪来？

3. 规范化规则的纪律是硬的：纯机械变换，禁止删字段、放宽约束、猜测语义。
   每条规则要有名字、单元测试、before/after 断言。规则数超过 10 条就停下来报告，
   那说明这条生成链不健康，该换工具而不是继续堆规则。

4. 第 4 步的 fixture 往返验证是本卡最有价值的部分。
   用生成的类型反序列化 T-004 的真实报文再序列化回去，跟原始 payload 做语义比较。
   能编译不代表能用 —— 字段可选性错了、枚举变体缺了，只有真实报文能暴露。
   失败条目逐条列出。不许改成 serde_json::Value 蒙混过关。

5. 结论是「做不到」也是合格交付，只要证据充分。
   主管会据此另开 ADR 决定换工具、缩子集、还是接受人工维护+漂移告警的妥协。
   不合格的是：为了打勾而放宽类型、删定义、把生成不了的东西悄悄移出子集。

交付时按 AGENTS.md §4.2 附带全部要求项。

边界：任务卡「边界」一节列出的文件一律不许碰。
特别注意 schemas/** 与 tests/fixtures/** 都是只读，一个字节都不许改。

现在开始。
```

</details>

---

## 全部跑完之后

请把五次交付的完整输出一起给我，我按 `CLAUDE.md §3` 的 Checklist 逐条审核，重点：

1. **测试真实性** —— 我会挑几处让 Codex 演示「把实现改坏，测试变红」
2. **fixture 是否真实** —— 对照 `schemas/` 校验，检查有没有手工编造的痕迹
3. **规范化规则是否越界** —— 逐条查有没有删字段、放宽约束、猜语义的变换
4. **是否越界建了 `crates/`** —— 产品 crate 的划分是 M2 的事
5. **fixture 里有没有敏感信息泄漏**

同时请把 G0 的 20 轮实测结果给我（T-001 交付后就可以开始跑，不必等 T-004）。
