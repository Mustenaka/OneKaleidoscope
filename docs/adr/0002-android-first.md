# ADR-0002: 移动端优先级调整为 Android 先行

- 状态：**已接受**（2026-07-28）
- 决策人：项目负责人
- 起草：项目主管
- 影响：`docs/REQUIREMENTS.md` §8（G4 / G6 互换）、`docs/MILESTONES.md`

---

## 背景

`REQUIREMENTS.md` §8 原定顺序为 **G4 = iOS App → G6 = Android 对齐**。

项目负责人决定：**优先出一版 Android，Android 稳定后再出 iOS。**

同时，门禁 G0 需要在「4G 手机网络」侧运行测量程序，而此时尚无 App。Android 可以通过交叉编译 + `adb push` + Termux 直接运行原生二进制；iOS 没有等价的低成本路径。这使得 Android 先行在 G0 阶段就已经是事实。

---

## 决策

### D-1 门禁顺序调整

| 原 | 新 | 说明 |
|---|---|---|
| G4 iOS App（F-1~F-3, F-7） | **G4 Android App（F-1~F-3, F-7）** | 仅用 Android 手机完成一次真实编码任务并合入 |
| G6 Android 对齐 | **G6 iOS 对齐** | 功能与 Android 一致，通过 F-1~F-10 |

G5（fs / git / diff 全量）保持在 G4 与 G6 之间，**在 Android 上验收**。

### D-2 G1 阶段的 UniFFI 绑定验证按平台分级

[ADR-0001](0001-technology-selection.md) D-6 要求 G1 就验证 UniFFI 绑定生成。按新顺序细化为：

- **Kotlin / AAR：必过项。** 必须生成成功并编译通过
- **Swift / XCFramework：必过项，但只要求「生成成功 + 编译通过」**，不要求接入 App

**iOS 的绑定生成不许推迟到 G6。** 理由：UniFFI 的表达能力限制（R-5）在 Swift 与 Kotlin 上表现不同（尤其 async 流与 enum with payload 的桥接）。如果只验证 Kotlin，等到 G6 才发现 Swift 侧表达不出来，`kaleido-proto` 已经被大量代码依赖，改动成本极高。**Android 先行指的是 UI 与真机验收先行，不是核心类型只对 Android 负责。**

### D-3 `kaleido-core` 的 API 不得出现 Android 专属妥协

任何为了绕开 UniFFI 限制而引入的设计（例如把 async 流降级为回调），必须同时在 Swift 侧验证可用。发现只在 Kotlin 侧成立的设计，一律打回。

---

## 后果

- `REQUIREMENTS.md` §8 的 G4 / G6 行互换
- `MILESTONES.md` 的任务卡编号与依赖关系按新顺序编排
- iOS 相关的平台能力（APNs、Keychain、`BGAppRefreshTask`）在 G6 才需要真机验证，但**推送载荷格式（§2 端 B 的零知识约束）必须在 G4 设计时就同时满足 APNs 与 FCM**，不许先按 FCM 设计再回头改
- G0 的 4G 侧默认在 Android 上运行（交叉编译 + adb push + Termux）

## 被否决的方案

| 方案 | 否决理由 |
|---|---|
| 两端并行开发 | 人力在真机测试上，并行会让两边都得不到充分验证 |
| 先 Android，iOS 的 UniFFI 绑定也推迟到 G6 | R-5 会在最贵的时候爆炸。见 D-2 |
| 砍掉 iOS | 与 REQUIREMENTS §2 冲突，负责人未提出 |

## 影响的门禁

- **G0**：明确 4G 侧运行在 Android 上
- **G1**：DoD 增加「Swift 与 Kotlin 绑定均生成成功并编译通过」
- **G4 / G6**：互换
- **G5**：验收平台改为 Android
