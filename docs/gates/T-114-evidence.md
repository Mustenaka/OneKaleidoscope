# T-114 QuestionSet evidence

> 状态：**T-114 本地合同门禁完成；不代表 R5 完成**
> 集成基线：`origin/main@c993d9f9bb115003e2ee69066c233ac47b7c52cc`
> 记录日期：2026-08-11

## 1. 合同结果

Question attention 现在由非空 `QuestionPrompt` 列表和逐题 `QuestionAnswer` 表达：

- 每题 stable `question_key`、options、`multi_select`、`free_form_allowed`；
- answer set 必须恰好覆盖请求题目；
- question/answer key、option ID 去重；
- 单选多答、未知 option、缺题、重复题、空答案、非法 free-form、Question/Decision shape 混用
  全部在 command admission 前拒绝；
- Approval/WorkflowGate 继续使用顶层 decision，拒绝 question answers；
- prompt 与 free-form 正文继续使用 Sensitive `ContentRef`，不进入 projection metadata/tracing；
- `AttentionAnswerSource::LocalCommand` 只来自真实本地 `CommandId`；无 command association 的
  provider/fixture traffic 使用 `ObservedExternal`。

最终 wire boundary 是 UACP `0.5.0`。中途使用的 `0.4.0` 从未合并或发布；`0.5.0` 同时承载
QuestionSet 与 mandatory
`AcceptedByRuntime { session_id, acceptance_kind, binding_handle }`。`PromptTurn` 必须关联同
command/session 的唯一 RemoteCommand Turn；`SessionControl` 用于 interrupt 等不创建 Turn 的
结构化控制 receipt。旧 `0.3.x`/`0.4.x` 都在业务 frame 前拒绝。

决策见 [ADR-0025](../adr/0025-question-set.md)。

## 2. focused 验证

已完成并通过：

- proto/state/store/core/transport 的 QuestionSet round-trip 与拒绝路径；
- Approval/WorkflowGate 回归；human decline 仍是正常终态，不是 Turn error；
- UniFFI API、Kotlin probe 与 Swift probe 编译；
- Android repository/mapper/UI 的逐题草稿、单/多选/free-form 与一次提交；
- Android main、androidTest source 编译与 unit tests；
- 旧 UACP minor 在业务 frame 前 fail-loud。

Android 验证使用本机已配置的 `<ANDROID_SDK>`；证据页不保存含用户名的绝对路径。
本卡的 Android 要求是共享合同消费/编译；T-113 仍要求实体设备上的三 provider 产品纵切。
旧 R5 SHA 的 no-build-cache 命令包含 `:app:compileDebugKotlin`、
`:app:compileDebugAndroidTestKotlin` 与 `:app:testDebugUnitTest`，结果 `BUILD SUCCESSFUL`。最新 R4/R5
集成候选已再次执行相同受影响层，实际构建 `arm64-v8a` + `x86_64` Rust/UniFFI 后通过 Kotlin、
androidTest source 与 JVM tests；`fc896be` 的 clean AAR/APK/lint/JVM 和 API 35 instrumentation
也已通过。全量 instrumentation 为 18 completed / 0 failed，其中 2 个实体专用 gate 按设计 skipped；
精确命令与边界由 T-113 记录。

## 3. 变异验证

答案覆盖校验曾被故意破坏，focused 测试实际变红；恢复实现后相同测试全绿。该记录证明
“恰好覆盖每题”的测试能失败，而不是只执行 happy path。

最终 DoD 把这条变异与 `cargo xtask ci` 绑定。最新 R4/R5 集成候选的完整本地 `cargo xtask ci` 已 exit 0，依次
通过 fmt、check-deps、lint-forbidden、clippy、Claude sidecar、workspace tests 与 fixtures verify，
因此 T-114 合同卡在合并 R4 后仍闭合。

## 4. R5 仍未完成的外部门禁

T-114 共享合同卡已完成；T-113 的 exact-commit 自动化子门禁已在 `58e1e9d` 关闭，但实体 arm64
Android 三 provider 纵切仍未通过。后者继续阻止 R5 完成，但不反向抹去本卡本地合同门禁。
