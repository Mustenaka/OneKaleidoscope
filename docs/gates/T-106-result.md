# T-106 验收结果

- 日期：2026-08-09
- 基线：`main@710d2d9`
- 分支：`codex/r3-android-lan-vertical-slice`
- 实现提交：`55baaf6`、`8cd949e`
- Pull Request：[PR #4](https://github.com/Mustenaka/OneKaleidoscope/pull/4)
- 结论：**通过。UACP 0.3 mobile contract 与 TRANSPORT 0.1 已形成可实现、fail-closed 的正式边界。**

## 1. 交付

- UACP 升至 `0.3.0`，新增 8 种闭合 `ProjectionKey`、独立 projection cursor 与订阅决策；
- 新增绑定可信 `DeviceId` 的远端命令请求，手机不能声明 Actor、command ID 或签发时间；
- 新增 ≤64 KiB 的敏感 `ContentWrite` 元数据合同，正文与控制帧分离；
- TRANSPORT `0.1.0` 固定 TLS 1.3、SPKI pin、一次性 QR 配对、P-256 设备认证、吊销、
  会话过期、资源上限、逐阶段 deadline 和 Host key 持久化规则；
- 幂等键改为无碰撞编码，side table 升级为 version 2 JSONL；旧无版本记录 fail-loud，避免静默重发 runtime；
- Kotlin 与 Swift UniFFI consumer 显式构造并穷举新增闭合类型。

## 2. 正负路径与变异

合同测试覆盖 projection resume/current/ahead/gap/overflow、key/payload 错配、重复/跳跃 cursor、
远端字段伪造、ContentWrite 大小/kind/digest/Sensitive 规则，以及旧幂等 side table 拒绝。

实际执行并恢复的变异包括：

1. 将协议版本退回 `0.2.0`，兼容测试变红；
2. 允许零字节 ContentWrite，大小边界测试变红；
3. RuntimeCapability 忽略 Host scope，key/payload 互验测试变红；
4. 恢复有分隔符碰撞的旧 dedupe 编码，注入性测试变红；
5. 将 cursor overflow 恢复为内部错误，wire `CursorGap` 测试变红。

全部变异均已恢复。

## 3. 门禁

本地最终运行：

```text
cargo xtask ci
exit 0

proto contract:       43 passed
state:                unit 7 + integration 22 passed
real fixtures:        5 fixtures / 220 records passed
clippy:               -D warnings passed
Kotlin consumer:      Gradle compileKotlin passed
```

新实现 SHA `8cd949e` 的 push 与 PR 两套三平台 CI 均全绿：

- [push run 31313726259](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/31313726259)
- [PR run 31313728415](https://github.com/Mustenaka/OneKaleidoscope/actions/runs/31313728415)

Ubuntu 编译 Kotlin UniFFI consumer，macOS 编译 Swift consumer，Windows 执行仓库门禁；
GitGuardian 同时通过。

## 4. 偏离与后续

- 未实现生产 listener、projection journal 或 Android UI，符合 T-106 边界；
- `CanonicalStore::projection` 返回 state-local diagnostic envelope，不伪造 mobile cursor；
- 真正 projection journal、LAN Broker 与 Rust mobile client 转入 T-107。
