# 任务索引

> 生效：2026-08-09。进度真源是 [STATUS.md](../STATUS.md)。

## 活动

| 卡 | 范围 | 状态 |
|---|---|---|
| [T-107](T-107.md) | hostd LAN broker、projection journal、配对/认证、Rust mobile core | **active** |

## 已排队

| 卡 | 范围 | 前置 |
|---|---|---|
| [T-108](T-108.md) | Android Compose 纵切与 emulator/真机验收 | T-107 |

T-106、T-107、T-108 是三个独立审核边界。不得为了让 Android 先出画面而在 Kotlin
重写 cursor、命令状态机、能力判断或 provider 协议。

## 已完成

| 卡 | 范围 | 结果 |
|---|---|---|
| [T-100](T-100.md) | Codex app-server → canonical state → durable log → Rust client | R2 通过，2026-07-31 |
| [T-102](T-102.md) | Swift/Kotlin UniFFI 四面编译探针 | 通过，2026-08-02 |
| [T-105](T-105.md) | Codex `0.147.0` schema drift 审查 | 通过，2026-08-09 |
| [T-103](T-103.md) | Attention answer provenance / UACP 0.2 | 通过，2026-08-09 |
| [T-104](T-104.md) | LiveControl runtime acceptance | 通过，2026-08-09 |
| [T-106](T-106.md) | UACP 0.3 mobile contract / TRANSPORT 0.1 | 通过，2026-08-09 |

## 冻结

- T-001～T-013 保留为历史研究记录；
- T-014 在实现前撤销；
- T-101 被 T-102 取代；
- 旧 prompt、unblock reply、repo-convergence 文件不再是活动任务。

新任务必须写清真实成功路径、错误路径、断线/恢复路径和变异测试；新增协议 wire 形状
必须同时更新 ADR、合同文本、Rust 类型与双端 UniFFI 编译门禁。
