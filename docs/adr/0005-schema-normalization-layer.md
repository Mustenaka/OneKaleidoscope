# ADR-0005: 上游 schema 规范化层与 `AGENTS.md §3.2` 修订

- 状态：**已接受**（2026-07-28）
- 起草：项目主管
- 触发：T-003 阻塞报告
- 影响：`AGENTS.md` §3.2、`docs/REQUIREMENTS.md` §4.3 / §4.4 / §9 R-4

---

## 背景

`AGENTS.md §3.2` 规定「类型不要手写」，并指定两条生成链：

| 来源 | 工具 |
|---|---|
| Codex app-server | `generate-json-schema` → `typify` |
| OpenCode | `/doc` 的 OpenAPI 3.1 → `progenitor` |

T-003 实测证明**这两条链对当前上游版本都不成立**：

### 事实 1：progenitor 不支持 OpenAPI 3.1

- OpenCode `1.18.8` 的 `/doc` 输出 OpenAPI **3.1.0**（478,637 bytes）
- progenitor `0.14.0` 的官方定位是「for generating opinionated clients from API descriptions in the **OpenAPI 3.0.x** specification」
- 具体撞击点：`exclusiveMinimum` 在 3.0 是**布尔**、在 3.1（JSON Schema 2020-12）是**数值**。OpenCode 的 `#/paths/~1pty~1{ptyID}/put/.../rows/exclusiveMinimum` 取值 `0`，progenitor 的 3.0 parser 期待布尔，直接解析失败

这不是配置问题，是规范版本的硬性不兼容。

### 事实 2：typify 无法处理 Codex 的完整 schema

- Codex `0.144.6` 的 `generate-json-schema` 导出 **267 个 draft-07 JSON**（2,720,160 bytes）
- typify `0.7.0` 在 `$ref #/definitions/v2/AdditionalFileSystemPermissions is missing` 处 panic —— 该 JSON Pointer 语义合法，但 typify 只索引顶层 `definitions`，不解析嵌套的 `definitions/v2/*`
- 仅取 v2 子集可以生成并 `cargo check` 通过，但**缺失 69 个 v1 定义**，其中包括 `ServerRequest`、审批请求、`Elicitation`、`JSONRPCMessage` —— 恰好是我们最需要的那些
- 单独生成官方 `ServerRequest.json` 触发 typify 的另一个未实现分支（**具体错误待补，见 T-005**）

### 事实 3：R-4 已经发生，不是未来风险

`REQUIREMENTS.md §9` 把「上游协议 breaking change 导致 adapter 编译失败」列为待缓解的风险，缓解措施是「类型从官方 schema 自动生成」。

现实是：**在写第一行 adapter 代码之前，自动生成这条路本身就已经断了。** R-4 需要按「已发生」重新定级。

---

## 决策

### D-1 `AGENTS.md §3.2` 的原则不变，路径修订

**「类型不许手写」这条铁律保留。** 手写上游类型仍然是打回项。

但在「上游 schema」与「生成器」之间，**允许插入一个规范化层（normalization layer）**：

```
上游 schema（原样快照，只读）
        ↓  规范化层：确定性、可测试、逐条记录的机械变换
规范化后的 schema（构建产物，不提交）
        ↓  生成器
Rust 类型（生成物）
```

### D-2 规范化层的纪律（违反即打回）

1. **`schemas/` 下的原样快照永不修改。** 它是 R-4 漂移监控的基准，也是「上游到底说了什么」的唯一凭证
2. 规范化产物写进 `target/` 或已 gitignore 的目录，**不提交**
3. **每条变换规则必须有名字、有单元测试、有 before/after 断言**。规则清单进 `docs/UPSTREAM.md`
4. **每条规则必须是纯机械的**：重命名、移位、等价改写。**禁止任何删除字段、放宽约束、猜测语义的变换**
5. 每次运行必须报告**每条规则的实际命中次数**。命中数为 0 的规则要删掉（说明上游已修，别留死代码）
6. **规则数量是健康度指标**：超过 10 条即视为该生成链不健康，必须回到主管重新评估工具选型，不许无限堆规则

### D-3 生成器选型改为「以证据决定」，不预先钦定

`AGENTS.md §3.2` 表格中的 `typify` / `progenitor` 由「必须使用」降级为「首选候选」。

**T-005 的任务是拿证据做选择，而不是让钦定的工具跑通。** 判定规则写在 T-005 卡里。

### D-4 只生成用得到的子集

Codex 导出 267 个 schema 文件，但 UACP 只需要其中一部分（thread / turn / item 生命周期、两类审批请求、elicitation、JSON-RPC 信封）。

**允许并鼓励只对需要的子集做生成。** 子集清单必须显式列出并说明理由 —— 这不是偷懒，是缩小攻击面；但**清单必须由 `PROTOCOL.md` 的需要推导，不许由「哪个能生成成功」倒推**。

后者是本 ADR 最容易被滥用的地方：如果发现某个类型生成不了就把它移出子集，等于用工具能力裁剪协议。**这种做法一律打回。**

---

## 被否决的方案

| 方案 | 否决理由 |
|---|---|
| 直接手写 Codex / OpenCode 的类型 | 违反 §3.2 核心原则。267 个定义手写必然漂移，且 R-4 完全失去缓解 |
| 修改 `schemas/` 下的快照使其可被生成器消化 | 会摧毁漂移监控的基准，让我们再也说不清「上游改了什么」vs「我们改了什么」 |
| 等 typify / progenitor 上游修复 | 时间不可控，且 R-4 是持续风险，不能把项目挂在别人的 issue 上 |
| 放弃 OpenCode 或 Codex 之一 | 与 REQUIREMENTS §4 冲突，负责人已明确三家全要 |
| 用 `codex app-server generate-ts` 走 TypeScript 中转 | 多一层语言转换，漂移风险更高，且 TS 类型到 Rust 仍需生成器 |

---

## 后果

- `AGENTS.md §3.2` 按 D-1 / D-3 修订
- T-003 descope 为「快照 + 漂移监控」，生成链落地拆为 **T-005**
- R-4 在 `REQUIREMENTS.md §9` 中重新表述为「已发生」
- **M2（协议定稿）不因此阻塞**：`PROTOCOL.md` 的设计依据是 T-004 的真实报文与 `schemas/` 的快照，不依赖 Rust 类型是否生成得出来

## 影响的门禁

- **G1**：不受影响（proto 是我们自己的类型，不是上游生成物）
- **G2**：受影响。三家 adapter 能否落地取决于 T-005 的结论。若某家最终只能人工维护类型子集，必须在 G2 前另开 ADR 明确记录该妥协及其漂移检测手段
