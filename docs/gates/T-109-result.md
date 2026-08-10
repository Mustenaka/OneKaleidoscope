# T-109 实体 arm64 Android 最终门禁结果

> 日期：2026-08-10
> 实现提交：`0a004d7bd07fadeb521fc01adf5997bac112d351`
> 结论：**通过，R3 完成**

## 1. 设备与网络

| 项 | 证据 |
|---|---|
| 设备 | Xiaomi `2410DPN6CC`，真实设备（`ro.kernel.qemu != 1`） |
| Android | SDK 36 |
| ABI | `arm64-v8a` |
| 连接 | PC 与手机同一真实 Wi-Fi LAN；未使用 `adb reverse` |
| Keystore | P-256 私钥不可导出；`KeyInfo` 为 TEE/StrongBox 硬件级别 |

原始设备序列号、DeviceId、pairing URI、正文与用户名路径未写入证据。机器可读 evidence 只保存
DeviceId 的 SHA-256 前 8 字节摘要。

## 2. 一键门禁

```powershell
.\scripts\android-physical-arm64-gate.ps1 `
  -BindAddress 192.168.31.139 `
  -BackgroundSeconds 90
```

提交 `0a004d7` 上最终命令 exit `0`，并产生以下闭合阶段：

1. `cargo xtask ci`：fmt、依赖、反模式、Clippy、workspace tests、5 个真实 fixture / 220 条记录全绿；
2. arm64-v8a + x86_64 UniFFI/APK/androidTest APK 构建与实体安装通过；
3. hardware-backed AndroidKeyStore 门禁通过；
4. 错误 SPKI pin 返回 Authentication，且未持久化凭据；
5. 新配对后七类 R3 projection 全部 Live；
6. Android 提交 prompt，真实 Codex `item/fileChange/requestApproval` 进入 Attention Inbox；
7. Android 按 runtime 原样提供的 destructive option decline，审批离开 inbox，隔离 probe 文件仍为 `ORIGINAL`；
8. 应用进入 OEM 后台 90 秒后重连，七投影恢复 Live；
9. 外部 `am force-stop` 后独立 instrumentation 冷启，读取后台阶段最后 cursor，重连 cursor 保持或前进且不回退；
10. host durable revoke 后再次冷启，设备进入 Revoked(Authentication)。

设备不允许测试 harness 强制 deep idle，因此 `device_idle_forced=false`；90 秒真实 OEM 后台通过，
并且本次设备进程 PID 在窗口前后保持一致。该事实没有被扩大成“深度 Doze 已验证”。

## 3. 错误路径与变异

- wrong pin 在同一实体机真实失败，vault 仍为空；
- durable revoke 后旧凭据真实失败；
- scratch 项目使用 `read-only + on-request`，避免 command-execution approval 与已支持的
  file-change approval 混淆；
- 临时把 `hardwareBacked` 强制为 false 后，实体 instrumentation 明确失败于
  `AndroidKeyStore P-256 identity is not hardware-backed`；恢复实现、重建、重装后 1/1 通过；
- PowerShell 脚本针对系统 5.1 修复并验证了 native Codex 发现、保留变量冲突、Gradle project/SDK、
  `ProcessStartInfo` 参数转义与 SHA-256 evidence 编码。

## 4. 机器可读摘要

最终 evidence JSON 记录：

```json
{
  "commit": "0a004d7bd07fadeb521fc01adf5997bac112d351",
  "manufacturer": "Xiaomi",
  "model": "2410DPN6CC",
  "android_sdk": "36",
  "abi_list": "arm64-v8a",
  "hardware_backed_keystore": true,
  "real_wifi_without_adb_reverse": true,
  "attention_declined_on_android": true,
  "declined_approval_left_probe_unchanged": true,
  "prompt_or_enqueue_accepted": true,
  "oem_background_seconds": 90,
  "device_idle_forced": false,
  "process_survived_background": true,
  "force_stop_cursor_resumed_exactly": true,
  "revoked_authentication_rejected": true
}
```

公网 relay/push、蜂窝网络切换与活进程树终止仍属于 R4，不在本门禁中伪报完成。
