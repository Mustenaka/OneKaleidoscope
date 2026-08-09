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
→ TransportHello / TransportHelloAck
→ UacpHello / UacpHelloAck
→ Pair 或 DeviceChallenge/Auth
→ authenticated business frames
```

```
TransportHello {
  request_id: u64,
  transport_version: String,
  max_frame_length: u32,
}

TransportHelloAck {
  request_id: u64,
  transport_version: String,
  max_frame_length: u32,
}

UacpHello { request_id: u64, protocol_version: String }
UacpHelloAck { request_id: u64, protocol_version: String }
```

TRANSPORT 0.1 的 `max_frame_length` 固定为 65,545。双方必须先接受 `0.1.x` transport，再接受
`0.3.x` UACP；ack 回显 responder 的完整版本和相同 frame 上限。minor 不兼容、畸形版本、
上限不等或顺序错误均返回 `VersionMismatch`（若可安全编码）并关闭连接。任何业务 frame 在
两次版本协商与设备认证完成前一律拒绝。R3 没有明文 fallback。

R3 的资源上限固定为：全局最多 64 条 TCP/TLS 连接，其中同时处于 pre-auth 的最多 16 条、
同一来源 IP 最多 4 条；认证后仍受每 DeviceId 2 条限制。TLS handshake 必须在 accept 后 5 秒
内完成，两次 hello 各 5 秒，pair/challenge auth 整体 30 秒；每个 frame 的 prefix+body read 与
每次 write 各 10 秒。认证连接 30 秒无业务时发送 ping，90 秒未收到任何合法 frame 则关闭。
超出 pre-auth/global 限额或阶段 deadline 时，在尚不能安全编码错误的阶段直接关闭；已协商错误
frame 且仍可写时返回 `RateLimited`。任何 timeout 都必须取消该连接的 pending request、未完成
upload 与 subscription，不能让慢连接持有无界 task/buffer。

Host TLS 私钥首次生成后持久保存：Unix 父目录 `0700`、key file `0600`；Windows DACL 只允许
当前用户与 SYSTEM 且关闭继承。写入必须使用同目录临时文件、file fsync、原子 rename 与目录
fsync（平台支持时）；私钥不得进入普通备份、日志或错误详情。文件缺失可首次生成，损坏/权限
放宽必须 fail-loud，不能静默换 key；显式轮换会改变 SPKI pin，必须撤销旧 pairing 并重新配对。

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

R3 的唯一 QR 文本编码是：

```text
onekaleidoscope://pair/v1?data=<base64url-no-pad(UTF-8 compact JSON)>
```

解码后的 JSON 最大 2,048 bytes，字段顺序固定为 `version`、`host_id`、`endpoint`、
`host_public_key_pin`、`secret`、`expires_at_ms`；`version` 必须为整数 `1`，ID 直接编码为
非空 String，`secret` 是恰好 32 bytes 的 base64url-no-pad（43 chars）。解析器拒绝未知/重复/
缺失字段、`=` padding、非 canonical base64url、尾随数据和其他 scheme/host/path/query key。
QR/URI 全文不得进入日志、剪贴板历史或普通 analytics。

endpoint 的 grammar 固定为 DNS hostname/IPv4 后接 `:` 与十进制 `1..=65535` 端口，或
`[IPv6-literal]:port`；IPv6 必须带方括号。禁止 userinfo、路径、query、fragment、空 host、
前导 `+`、端口前导零、IPv6 zone ID 与非 ASCII hostname。连接成功后的 SPKI pin 才是 Host
身份，endpoint 本身不参与授权。

`host_public_key_pin` 的唯一合法编码是
`sha256:` + `base64url-no-pad(SHA-256(DER SubjectPublicKeyInfo))`：前缀后恰好 43 个
base64url 字符，无 `=` padding。客户端从 TLS 1.3 peer certificate 提取 DER SPKI 后计算
digest，解码 pin 并恒时比较 32 bytes；格式错误或不相等都在发送 secret 前终止 TLS。它不是
certificate pin；系统/用户 CA 或 hostname 验证不能替代该精确 SPKI pin。

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
  connection_id: String,
  session_expires_at_ms: i64,
}
```

公钥必须是 P-256 SPKI；label 去首尾空白后长度为 `1..=80`，只用于显示。

secret 错误、已过期、已使用或不存在，对 wire 一律只返回
`TransportErrorCode::PairingInvalid` 并应用同一限速；不得用错误码或日志区分目录状态。host
必须对收到的 secret 计算 SHA-256 后，与目录 digest 恒时比较。
成功路径必须在返回 PairResponse 前，把 secret 单次消费与新 DeviceId/public key 目录记录作为
同一持久事务提交并 fsync。PairResponse 发送后当前连接即认证为该 DeviceId，无需再走
Challenge；`connection_id` 与 session expiry 遵循 §3。host 内部可以分别记录安全计数，
但日志不得包含 secret/digest。

## 3. 重连认证

```text
ChallengeRequest  { request_id, device_id }
DeviceChallenge   { request_id, challenge_id: Vec<u8>, nonce: Vec<u8>, expires_at_ms }
ChallengeProof    { request_id, challenge_id: Vec<u8>, signature_der: Vec<u8> }
AuthAccepted      { request_id, connection_id: String, expires_at_ms }
```

`challenge_id` 恰好 16 random bytes；`nonce` 恰好 32 random bytes。challenge 默认 30 秒
到期、单次使用，成功或任一失败后立即从可用集合移除。`connection_id` 是 host 分配的非空
opaque canonical ID；短期 session credential 只存在于内存，不写磁盘。PairResponse 与
AuthAccepted 的 session lifetime 固定为 15 分钟。

TLS channel binding 固定为 TLS exporter：label
`EXPORTER-OneKaleidoscope-R3-DeviceAuth`、无 context、输出 32 bytes。签名 transcript 是以下
字节的无分隔拼接：

```text
ASCII("OneKaleidoscope.DeviceAuth.v1")
u16be(len(transport_version UTF-8)) || transport_version UTF-8
u16be(len(UACP_version UTF-8))      || UACP_version UTF-8
u16be(len(HostId UTF-8))           || HostId UTF-8
u16be(len(DeviceId UTF-8))         || DeviceId UTF-8
tls_exporter[32]
challenge_id[16]
nonce[32]
i64be(expires_at_ms)
```

四个变长字段按 UTF-8 byte length 编码，非空 ID、版本格式与已协商值必须先验证；任一长度超过
`u16::MAX`、固定数组长度不符、expiry 已到或不属于该 challenge 都拒绝。设备用 Android
Keystore P-256 private key 对完整 transcript 做 ECDSA-SHA256，`signature_der` 必须是严格 DER；
host 用目录中该 DeviceId 的 SPKI 验证。字段替换、TLS 连接变化、nonce/challenge 重放、错误
key 与已吊销 DeviceId 都不得认证成功。

ChallengeRequest 中未知或已吊销的 DeviceId、错误设备公钥/签名，对 pre-auth wire 一律返回
`AuthenticationFailed` 并使用同一限速/外部时序；不得暴露设备目录。`ChallengeExpired` 与
`ChallengeReplayed` 只可用于当前 TLS 连接上确实签发过的 challenge。`DeviceRevoked` 只用于
已经认证的连接收到 durable-first 吊销通知。

session 到达 `session_expires_at_ms` 后，host 必须先停止接收新业务 frame，终止未提交请求与
subscription，发送 `AuthenticationFailed { retriable = true }`（若可安全写入），随后 TLS
`close_notify` 并关闭；TRANSPORT 0.1 不支持连接内 re-auth。已经进入 canonical durable write
path 的命令不回滚。客户端必须新建 TLS 连接并重新 challenge，不能延长旧 expiry。

## 4. Frame

每个 TLS application frame：

```text
u32 big-endian frame_length
u8 frame_kind                       // 0x01 = JSON control, 0x02 = content body
frame body

JSON control body  = frame_length - 1 bytes of UTF-8 JSON
content body       = u64 big-endian request_id + 1..=65536 raw bytes
```

- JSON control body 最大 65,536 bytes；content raw bytes 最大 65,536 bytes。因此接收端在分配
  前接受的 `frame_length` 上限固定为 65,545（1-byte kind + 8-byte request ID + body）；
- 长度与 kind 必须在读取/分配前验证；JSON control body 不得为空，content raw bytes 必须在
  `1..=65536`；
- 零长、截断、无效 UTF-8/JSON、未知 `frame_kind`、重复 request ID 均关闭连接；
- 所有数据 enum 闭合，未知 kind 不降级；
- 每连接最多 32 个 pending request、16 个 active subscription；每 DeviceId 最多 2 个连接。

所有多字节整数使用 big-endian。content frame 的 request ID 必须对应一个已接受、正在等待
正文且尚未绑定 binary frame 的 ContentWrite 控制头；否则关闭连接。解析器不得先按声明长度
分配，再检查 65,545 上限。

## 5. 业务 frame

每个 JSON control body 是一个内部标签闭合 enum，标签键固定为 `kind`、变体名为
`snake_case`。除 hello/pair/auth 的 §1～§3 record 外，认证后允许的业务变体只有：

```text
ProjectionSubscribeFrame {
  request_id: u64,
  subscription_id: u64,
  subscribe: ProjectionSubscribe,
}
ProjectionSubscribeAckFrame {
  request_id: u64,
  subscription_id: u64,
  ack: ProjectionSubscribeAck,
}
ProjectionEnvelopeFrame {
  subscription_id: u64,
  envelope: ProjectionEnvelope,
}
UnsubscribeRequest { request_id: u64, subscription_id: u64 }
UnsubscribeAck     { request_id: u64, subscription_id: u64 }

ContentWriteHeader { request_id: u64, request: ContentWriteRequest }
ContentWriteResult { request_id: u64, response: ContentWriteResponse }
ContentReadFrame   { request_id: u64, request: ContentReadRequest }
ContentReadResult  { request_id: u64, response: ContentReadResponse }

DeviceCommandFrame { request_id: u64, request: DeviceCommandRequest }
DeviceCommandAck   { request_id: u64, ack: CommandAck }

Ping { request_id: u64, nonce: u64 }
Pong { request_id: u64, nonce: u64 }
TransportError
```

`request_id` 和 `subscription_id` 必须非零，在一条连接的每个发送方向由请求方单调分配且不得复用；响应
逐字回显 request ID，projection push 逐字回显 subscription ID。任一计数器到 `u64::MAX` 后，
发送方必须在需要下一 ID 前正常关闭并新建连接；不得 wrap、归零或复用。未知 control `kind`、响应 ID
错配、subscription ID 冲突或已 unsubscribe 后继续 push 都是 `MalformedFrame` 并关闭连接。

ContentWrite 的 `ContentWriteRequest { content_kind, byte_len, digest }` 是 JSON 控制头；正文
使用其后、同一 request ID 的唯一 binary content frame，不内嵌 JSON，也不进入 JSON
日志。缺失、重复、request ID 错配、声明长度/digest 与实际 bytes 不一致都关闭该请求且不得
写入内容存储。其他业务载荷直接使用 UACP JSON 编码。

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

ContentWrite 只允许 PlainText/Markdown，声明与实际 `byte_len` 均在 `1..=65536`。host 对
binary body 复算长度与 digest，并强制响应 `ContentRef` 为 Sensitive、无 preview、Stored；
不能信任控制头声明。ContentRead 继续按 UACP chunk/digest 规则；任何正文、签名、secret、
私钥、完整路径不得进入普通 tracing、push 或 transport metadata。

## 8. 吊销与错误

Device registry 至少保存 DeviceId、公钥、创建时间、吊销时间。吊销立即关闭该设备连接并
阻止重新认证。

吊销只允许本机 hostd 管理命令或受信本地 API 发起，不存在 LAN revoke/self-service frame。
host 必须先在一个事务中写入并 fsync `revoked_at_ms`，之后才：

1. 对仍可安全写入的连接发送 `TransportError { code = DeviceRevoked }`；
2. 终止该 DeviceId 的所有 subscription、未提交 request 与不完整 content upload；
3. 对全部连接发送 TLS `close_notify` 并关闭；
4. 拒绝该 DeviceId 后续所有 challenge。

已经进入 canonical durable write path 的命令不回滚；尚未提交的请求不得在吊销后继续进入
Broker。进程在 fsync 后、断连前崩溃时，重启必须先读到 revoked 状态并拒绝认证。

```
TransportError {
  request_id: Option<u64>,
  code: TransportErrorCode,
  retriable: bool,
}
```

闭合 TransportErrorCode：

```text
VersionMismatch | MalformedFrame | FrameTooLarge | RateLimited
PairingInvalid
AuthenticationFailed | ChallengeExpired | ChallengeReplayed | DeviceRevoked
TooManyConnections | TooManySubscriptions | Internal
```

`PairingInvalid` 覆盖 secret 错误、过期、已消费和不存在；wire 不得出现更细的配对错误码。
`request_id` 仅在已经安全解析到非零 ID 时为 Some。error 没有 detail/string/source 字段，不包含
secret、signature、endpoint、路径或自由文本详情。TLS/pin 失败与无法安全解析的 pre-auth frame
直接关闭；已认证结构错误可以先发送一条安全 `TransportError`，随后必须关闭。认证后的
canonical 业务拒绝仍使用 UACP `CanonicalError`，不得伪装成 provider 错误。

## 9. 日志与测试红线

- 日志允许 DeviceId、connection ID、错误码、计数和时间；
- 日志禁止 pairing secret/digest、Host pin、TLS exporter、challenge ID/nonce、私钥、公钥原文、
  签名、正文、完整 endpoint/path；
- 任何跳过 pin、鉴权、长度上限、cursor checked arithmetic 或吊销检查的变异必须让测试变红；
- pairing invalid/expired/used 返回不同 wire code、先断连后持久化吊销、或在签名 transcript
  漏掉任一字段的变异也必须让测试变红；
- transport 不读取终端、PTY、ANSI 或 provider transcript 文件。
