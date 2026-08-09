# OneKaleidoscope TRANSPORT 0.1

> 状态：R3 LAN 合同，2026-08-09  
> `TRANSPORT_VERSION = "0.1.0"`  
> 决策来源：[ADR-0021](adr/0021-r3-lan-security.md)

本文只定义 PC hostd ↔ mobile core 的安全连接、frame、配对和设备认证。canonical 业务类型
仍由 [PROTOCOL.md](PROTOCOL.md) / `kaleido-proto` 定义。

## 1. 连接顺序

```text
TCP connect
→ TLS 1.3 + exact Host public-key pin
→ TransportHello(transport_version, max_frame_bytes)
→ Pair 或 DeviceChallenge/Auth
→ UacpHello(protocol_version)
→ authenticated business frames
```

业务 frame 在两次版本协商与设备认证完成前一律拒绝。R3 没有明文 fallback。

## 2. Pairing bootstrap

bootstrap 是一次性 QR/URI 载荷：

```text
PairingBootstrap {
  host_id: HostId,
  endpoint: String,
  host_public_key_pin: String,
  secret: Vec<u8>,              // exactly 32 bytes
  expires_at_ms: i64,
}
```

endpoint 只允许 `host:port`，不得含用户名、query 或业务路径。secret 只在 bootstrap 明文中
出现一次；host 只保存 SHA-256 digest，默认 5 分钟、原子单次消费。

手机验证 pin 后发送：

```text
PairRequest {
  request_id: u64,
  secret: Vec<u8>,
  device_public_key_spki: Vec<u8>,
  device_label: String,
}

PairResponse {
  request_id: u64,
  device_id: DeviceId,
  host_id: HostId,
  transport_version: String,
  protocol_version: String,
}
```

公钥必须是 P-256 SPKI；label 去首尾空白后长度为 `1..=80`，只用于显示。

## 3. 重连认证

```text
ChallengeRequest  { request_id, device_id }
DeviceChallenge   { request_id, challenge_id, nonce, expires_at_ms }
ChallengeProof    { request_id, challenge_id, signature_der }
AuthAccepted      { request_id, connection_id, expires_at_ms }
```

签名输入使用固定域分隔并按字段长度编码：transport version、UACP version、HostId、DeviceId、
TLS channel binding、challenge ID、32-byte nonce、expiry。任何字段替换、nonce 重放或过期都
必须让验签失败。challenge 成功或失败后都立即作废。

## 4. Frame

每个 TLS application frame：

```text
u32 big-endian byte_length
byte_length bytes of UTF-8 JSON control frame or declared binary content frame
```

- 控制 frame 与 content frame 均最大 65,536 bytes；
- 长度必须在读取/分配前验证；
- 零长、截断、无效 UTF-8/JSON、未知 `kind`、重复 request ID 均关闭连接；
- 所有数据 enum 闭合，未知 kind 不降级；
- 每连接最多 32 个 pending request、16 个 active subscription；每 DeviceId 最多 2 个连接。

## 5. 业务 frame

认证后允许：

- `ProjectionSubscribe` / `ProjectionSubscribeAck`；
- `ProjectionEnvelope`；
- `ContentWriteRequest/Response`；
- `ContentReadRequest/Response`；
- `DeviceCommandRequest` / `CommandAck`；
- ping/pong、unsubscribe、structured transport error。

ContentWrite 的 bytes 使用 binary content frame，通过 request ID 与控制头关联，不进入 JSON
日志。其他业务载荷直接使用 UACP JSON 编码。

## 6. Projection subscription

订阅以 `ProjectionKey` 为单位。服务端在同一锁/actor 顺序中：

1. 注册 live tail；
2. 读取 journal floor/head 与 since；
3. 返回 Resumed 或 CurrentFollows；
4. 发送保留窗口或当前完整 projection；
5. 只发送大于已发送 head 的 live entry。

这个顺序保证 snapshot/replay 与 live 交界不漏不重。慢客户端一旦落后 bounded channel，服务端
发送 CursorGap（若仍可写）并关闭订阅；不得跳 cursor。

## 7. Command 与 content

DeviceCommandRequest 不含 Actor、CommandId、issued_at。hostd 从已认证 DeviceId 构造
canonical envelope；连接身份和请求摘要进入持久幂等/outbox 事务。

ContentWrite 只允许 PlainText/Markdown、≤64 KiB。host 计算 digest 并强制 Sensitive、无
preview。ContentRead 继续按 UACP chunk/digest 规则；任何正文、签名、secret、私钥、完整路径
不得进入普通 tracing、push 或 transport metadata。

## 8. 吊销与错误

Device registry 至少保存 DeviceId、公钥、创建时间、吊销时间。吊销立即关闭该设备连接并
阻止重新认证。

闭合 TransportErrorCode：

```text
VersionMismatch | MalformedFrame | FrameTooLarge | RateLimited
PairingInvalid | PairingExpired | PairingAlreadyUsed
AuthenticationFailed | ChallengeExpired | ChallengeReplayed | DeviceRevoked
TooManyConnections | TooManySubscriptions | Internal
```

外部错误不包含 secret、signature、endpoint、路径或自由文本详情。认证后的 canonical 业务拒绝
仍使用 UACP `CanonicalError`。

## 9. 日志与测试红线

- 日志允许 DeviceId、connection ID、错误码、计数和时间；
- 日志禁止 pairing secret/digest、私钥、公钥原文、签名、正文、完整 endpoint/path；
- 任何跳过 pin、鉴权、长度上限、cursor checked arithmetic 或吊销检查的变异必须让测试变红；
- transport 不读取终端、PTY、ANSI 或 provider transcript 文件。
