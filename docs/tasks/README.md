# 任务索引

> 生效：2026-08-10。进度真源是 [STATUS.md](../STATUS.md)。

## 活动

| 卡 | 范围 | 状态 |
|---|---|---|
| [T-110](T-110.md) | R4 自有 Ubuntu rendezvous/P2P/relay、跨公网 E2EE、FCM、网络切换、活进程树终止 | active |
| [T-111](T-111.md) | R5 OpenCode REST/SSE、D-B11、生成类型、真实会话与恢复 | active；question fixture 通过，latest generated live 因 `/doc`/`/event` 漂移 blocked |
| [T-112](T-112.md) | R5 Claude Agent SDK Broker、真实流式会话、permissions 与恢复 | active；真实 OAuth 失败，success 未验 |
| [T-113](T-113.md) | R5 多 provider hostd/移动端集成、跨平台 CI 与实体 Android 总门禁 | active；依赖 T-111/T-112 |
| [T-114](T-114.md) | UACP QuestionSet、多选/自由文本回答与 Android 共享交互 | completed；本地总门禁通过，不代表 R5 完成 |

## 已完成

| 卡 | 范围 | 结果 |
|---|---|---|
| [T-100](T-100.md) | Codex app-server → canonical state → durable log → Rust client | R2 通过，2026-07-31 |
| [T-102](T-102.md) | Swift/Kotlin UniFFI 四面编译探针 | 通过，2026-08-02 |
| [T-105](T-105.md) | Codex `0.147.0` schema drift 审查 | 通过，2026-08-09 |
| [T-103](T-103.md) | Attention answer provenance / UACP 0.2 | 通过，2026-08-09 |
| [T-104](T-104.md) | LiveControl runtime acceptance | 通过，2026-08-09 |
| [T-106](T-106.md) | UACP 0.3 mobile contract / TRANSPORT 0.1 | 通过，2026-08-09 |
| [T-107](T-107.md) | hostd LAN broker、projection journal、配对/认证、Rust mobile core | 通过，2026-08-10 |
| [T-108](T-108.md) | Android Compose 纵切与 emulator 验收 | 通过，2026-08-10 |
| [T-109](T-109.md) | 实体 arm64 Android 最终门禁 | R3 通过，2026-08-10 |

## 冻结

- T-001～T-013 保留为历史研究记录；
- T-014 在实现前撤销；
- T-101 被 T-102 取代；
- 旧 prompt、unblock reply、repo-convergence 文件不再是活动任务。

新任务必须写清真实成功路径、错误路径、断线/恢复路径和变异测试；新增协议 wire 形状
必须同时更新 ADR、合同文本、Rust 类型与双端 UniFFI 编译门禁。
