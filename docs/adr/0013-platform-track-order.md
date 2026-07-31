# ADR-0013: 平台推进顺序 Windows + Android 先行，Swift 编译门禁改为携带式阻塞

- 状态：**已接受，2026-07-30**
- 决策人：项目负责人（平台顺序）+ 项目主管（门禁处置）
- 影响：[MILESTONES](../MILESTONES.md) R1 / R8 门禁归属；[R1 评审](../gates/R1-result.md) 结论
- 不影响：[REQUIREMENTS](../REQUIREMENTS.md) OBJ-10、§7.1 第 6 条、§8 验收矩阵

## 背景

R1 恢复交付后，唯一未取得机器证据的门禁是 **Swift UniFFI 绑定编译**：

- Swift 绑定**生成成功**（`kaleido_proto.swift` 357744 B、`kaleido_core.swift` 22538 B，
  外加两份 FFI header 与 modulemap），生成器没有报告任何 unsupported / skipped 类型；
- Kotlin 绑定生成并**实际编译通过**（842 个 class，`KOTLIN_PROBE_EXIT=0`）；
- 开发机是 Windows 11，本机与 WSL Ubuntu-24.04 均无 `swiftc` / `swift` / `xcrun`，
  Docker Linux engine 未运行。

项目负责人确认的开发顺序是：**先 Windows PC + Android，随后再扩展验证到
macOS + Android + iOS**。

## 决策

### D-1 v1 的平台推进顺序固定为两轨

| 轨 | 内容 | 里程碑归属 |
|---|---|---|
| 主轨 | Windows hostd + Android App | R2 → R3 → R4 → R5 → R6 |
| 后置轨 | macOS/Linux hostd + iOS App | R8 起，含 macOS 打包在 R9 |

`REQUIREMENTS` OBJ-10（PC 支持 Windows/macOS/Linux，手机支持 Android/iOS）**不变**。
本 ADR 只固定验证顺序，不删除任何平台。

### D-2 R1 的「移动端绑定编译」门禁拆成两项，Swift 项改为携带式阻塞

| 门禁项 | 结论 | 归属 |
|---|---|---|
| R1-K：Kotlin 绑定对真实 canonical 类型编译通过 | 已通过，有机器证据 | R1 |
| R1-S：Swift 绑定对真实 canonical 类型编译通过 | **未通过，登记为 UB-R1-S** | **携带至 R8 入口，且是 R8 的硬前置** |

R1 因此记为 **有条件通过（携带 UB-R1-S）**，不是「通过」。任何文档、评审或交付说明
都不得把 R1 写成无条件通过，也不得把「Swift 生成成功」写成「Swift 编译通过」。

### D-3 UB-R1-S 的解除路径已确定，且不需要负责人拥有 Mac

`.github/workflows/ci.yml` 的矩阵**已经包含 `macos-latest`**。解除路径是：在该 job 上
增加一个仅 macOS 执行的步骤，生成 Swift 绑定并用 runner 自带的 Swift 工具链编译
`crates/kaleido-core/bindings/swift-probe/Probe.swift`。这是真实机器证据，
不是把要求改小。

解除条件：某次可追溯到具体 commit 的 macOS CI 运行中，该步骤成功且未被 `continue-on-error`
之类的开关软化。见 [T-102](../tasks/T-102.md)。

### D-4 Swift 门禁不阻塞 R2 与 R3

理由必须写清楚，避免以后被当成惯例：

1. R1-S 校验的是**同一份 canonical 类型**能否被第二种目标语言消化。生成器已经在
   同一批类型上完成 Swift 代码生成且无 unsupported 项，剩余风险集中在编译期细节
   （命名冲突、模块可见性、平台 header），不改变 canonical 模型本身；
2. R2（`kaleido-state` / `kaleido-adapter` / `kaleido-adapter-codex` / `kaleido-hostd`）
   是纯 Rust 本机纵切，不产生任何 Swift 代码；
3. R3 是 Android，消费的是已经编译通过的 Kotlin 绑定。

若 T-102 的结果是「Swift 编译失败且需要修改 canonical 类型」，则 R1 退回未通过，
按 `AGENTS.md` §5 停下来由主管改协议，R2/R3 的既有产出按新协议返工。这个风险被
接受，因为它的代价远小于把主线停在等一台 Mac。

## 后果

- [MILESTONES](../MILESTONES.md) R1 记为有条件通过；R8 入口新增 UB-R1-S 前置；
- [T-100](../tasks/T-100.md) 解除 blocked，成为唯一活动实现任务；
- [T-102](../tasks/T-102.md) 承担 UB-R1-S 解除与 UniFFI 订阅面探针，**必须在 R3 开工前完成**；
- [REQUIREMENTS](../REQUIREMENTS.md) §8 六格验收矩阵不变，仍然全部「待验收」。

## 被否决的方案

| 方案 | 否决理由 |
|---|---|
| 在本机装 Swift for Windows / WSL Swift 后再继续 | 引入一条与目标平台（iOS/macOS）不同的工具链，编译通过也不能代表 Apple 平台可用，属于假证据 |
| 把 R1-S 删掉或改写成「生成成功即通过」 | `CLAUDE.md` §2 明令禁止降低需求宣布完成 |
| 让整个项目停在 R1 等一台 Mac | `CLAUDE.md` §7：阻塞不等于停项目，主管应推进独立纵切并保留失败验收格 |
| 把 iOS 移出 v1 | 与 [REQUIREMENTS](../REQUIREMENTS.md) OBJ-10、§7.1 冲突，负责人也没有提出该要求 |
