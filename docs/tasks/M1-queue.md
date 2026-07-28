# M1 执行队列 —— 依次粘贴给 Codex

> 里程碑：**M1 地基与实测材料**（见 [MILESTONES.md](../MILESTONES.md)）
> 用法：按顺序执行。**每一条等 Codex 交付完再发下一条**，因为后一张卡依赖前一张的产物。
> 全部四条完成后整批交回主管审核。

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

## Prompt 3 / 4 —— T-003 上游 schema 快照与漂移监控

```
继续 OneKaleidoscope 项目，你仍是实现方（Implementer）。

动手前必读：
1. AGENTS.md —— 全文重读，尤其 §3.2（类型生成，不要手写）
2. docs/tasks/T-003.md —— 本次任务卡
3. docs/REQUIREMENTS.md §4（三家接入规格）、§9 R-4
4. docs/adr/0004-acp-version-pinning.md —— ACP 钉 v1 / crate 1.x，不跟 v2 Draft

本次任务：执行 docs/tasks/T-003.md。

需要特别注意的：

1. 版本号一律钉死确切值，不许写 latest 或 ^。schemas/VERSIONS.md 必须记录
   每份 schema 的确切版本号与逐字可复制的抓取命令 —— 别人照着敲要能复现出同一份文件。

2. diff 必须是语义化的（解析 JSON 后比较），键顺序变化不算漂移。
   DoD 要求你人为改一个字段验证它会被检出并指出 JSON 路径，把输出粘出来。

3. 生成器冒烟是这张卡最重要的部分。如果 typify 吃不下 Codex 的 schema，
   或 progenitor 吃不下 OpenCode 的 spec，或生成物编译不过 ——
   这是必须上报的重大发现，它意味着 AGENTS.md §3.2 在该家 agent 上不成立。
   按 AGENTS.md §5 报告，不要自己改成手写类型绕过去。

4. 生成的代码不要提交进仓库。

人类前置条件：本机需已安装并可运行 codex 与 opencode。缺失请立即报告阻塞，不要跳过该家。

交付时按 AGENTS.md §4.2 附带全部要求项，并额外报告三家的确切版本号
与对 R-4 缓解可行性的结论。

边界：任务卡「边界」一节列出的文件一律不许碰。本任务不创建 crates/ 下的任何东西。

现在开始。
```

---

## Prompt 4 / 4 —— T-004 三家 agent 真实事件录制

```
继续 OneKaleidoscope 项目，你仍是实现方（Implementer）。这是 M1 的最后一张卡。

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

---

## 四条跑完之后

请把四次交付的完整输出一起给我，我按 `CLAUDE.md §3` 的 Checklist 逐条审核，重点：

1. **测试真实性** —— 我会挑几处让 Codex 演示「把实现改坏，测试变红」
2. **fixture 是否真实** —— 对照 `schemas/` 校验，检查有没有手工编造的痕迹
3. **是否越界建了 `crates/`** —— 产品 crate 的划分是 M2 的事
4. **fixture 里有没有敏感信息泄漏**

同时请把 G0 的 20 轮实测结果给我（T-001 交付后就可以开始跑，不必等 T-004）。
