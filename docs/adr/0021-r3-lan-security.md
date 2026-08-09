# ADR-0021：R3 LAN 使用 pinned TLS 与 Keystore 设备身份

- 状态：**已接受，2026-08-09**
- 决策人：用户（项目主管）
- 任务：[T-106](../tasks/T-106.md)
- 补充：[ADR-0011](0011-self-hosted-connectivity.md)

## 背景

R3 要求 Android 与 hostd 在局域网完成配对、实时订阅、命令和断线恢复；但 UACP 0.2 把
分帧、配对和 E2EE 整体留给 R4。如果临时开放明文 HTTP、固定 token 或无鉴权 socket，
局域网里的其他设备即可读取项目元数据或发审批命令。

R3 不需要提前实现 Ubuntu rendezvous/relay/push，但必须给 PC↔手机直连提供真实机密性、
Host 身份、设备身份与吊销。

## 决策

### D-1 独立 TRANSPORT 0.1

transport 版本独立于 UACP，首版为 `0.1.0`。连接先完成 transport handshake，再交换
UACP 版本；任一版本不兼容都在业务 frame 前拒绝。

R3 采用 TLS 1.3。hostd 只开放加密 listener，不存在明文 downgrade 或“仅本地调试”业务端口。
frame、pairing、auth、rate limit 错误使用 transport 自己的闭合错误，不混用 provider
CanonicalError。

### D-2 Host pin 与一次性邀请

hostd 首次启动生成持久 TLS identity。配对 bootstrap/QR 只包含：HostId、LAN endpoint、
Host public-key pin、256-bit 随机 secret 与 expiry。

- secret 在 host 只存 digest；
- 默认 5 分钟过期；
- 成功消费必须原子、单次；
- 错误、过期和重复消费使用同样的外部错误粒度并限速；
- bootstrap、secret、证书私钥不得进入 tracing。

手机必须先验证 Host pin，之后才发送 pairing secret，防止把 secret 交给中间人。

### D-3 Android Keystore 设备身份

Android 在 Keystore 生成不可导出的 P-256 signing key。配对时上传公钥，host 分配 DeviceId
并持久化公钥/状态。显示名只用于 UI，不参与授权。

重连认证绑定：TLS channel、Host nonce、DeviceId、transport version、UACP version 和
请求时间窗。nonce 单次、短时；签名重放、错误 key、已吊销 DeviceId 均拒绝。认证成功后的
短期 session credential 只在内存存在。

吊销持久化，并让该 DeviceId 的现有连接和订阅立即失效。

### D-4 有界 frame 与连接资源

- 控制 frame 最大 64 KiB；ContentWrite 单次正文最大 64 KiB；
- 长度前缀在分配前 checked，零长、超限、截断和未知 frame kind fail-closed；
- 每设备连接、订阅和并发请求有固定上限；
- 慢订阅者不能让 Broker 丢 projection，发生 lag 时以 CursorGap 断开；
- 所有 request/response 有 correlation ID，但日志只记录安全的 canonical DeviceId、错误码、
  计数和时间，不记录 body、secret、签名或完整 endpoint/path。

### D-5 R4 复用边界

R3 TLS 是手机到 PC 的端到端加密。R4 的 P2P/relay 只提供连接建立和密文字节流转发，不能
终止业务加密或读取 UACP frame。若未来 TLS-over-relay 不能满足网络迁移，再以新 ADR 替换，
不得在 R3 私加第二套明文 framing。

## 拒绝的方案

- 明文 HTTP/WebSocket + bearer token：泄漏正文/命令且无 Host 身份；
- pairing secret 长期作为设备 credential：无法做到硬件密钥与细粒度吊销；
- 信任局域网来源 IP：DHCP、NAT 和共享 Wi-Fi 都不是设备身份；
- 把证书错误设为“继续连接”：pin 失去意义；
- 在 Kotlin 实现 UACP/cursor：违反共享核心边界。

## 后果

- T-107 需要新增 `kaleido-transport`、TLS/P-256/随机数依赖和设备目录；
- Android Kotlin 只实现 Keystore signer callback 与平台存储；
- 本地 emulator 可以完整验证 pin、pair、auth、revoke 与 reconnect；
- 实体 Wi-Fi、OEM 后台和 hardware-backed Keystore 仍是 T-108 最终门禁。
