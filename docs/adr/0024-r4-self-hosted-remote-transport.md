# ADR-0024：R4 使用自有 iroh relay 承载内层 pinned TLS

- 状态：**已接受，2026-08-10**
- 决策人：Codex（按 T-110 与仓库协作合同直接实施）
- 任务：[T-110](../tasks/T-110.md)
- 补充：[ADR-0011](0011-self-hosted-connectivity.md)、
  [ADR-0020](0020-projection-cursors-and-mobile-ingress.md)、
  [ADR-0021](0021-r3-lan-security.md)

## 背景

R3 的 `TRANSPORT 0.1` 已经给手机到 PC 提供 TLS 1.3、精确 Host SPKI pin、Android
Keystore P-256 设备身份、durable revoke、有界 frame 和 projection cursor 恢复。R4 需要在
家庭路由器不开放入站端口的条件下增加公网 P2P、自有 Ubuntu relay 和 push，但不得让 Ubuntu
取得业务密钥，也不得让网络切换重新解释 UACP 或重置 projection cursor。

冻结 spike 使用 iroh 1.0.3，但它只证明过 NAT 测量路径，不能作为生产合同。2026-08-10 重新核对
iroh 1.0.3 官方稳定文档与源码后确认：

- `Endpoint` 使用 EndpointId 对 QUIC/TLS 1.3 对端做认证，支持 P2P、打洞、relay 回退与路径迁移；
- `RelayMode::Custom` 可以只配置自有 relay；默认模式会连接 n0 公共 relay，因此产品代码不得使用；
- `iroh-relay` 1.0.3 提供可自托管 server、认证 `AccessControl`、客户端带宽限制和运行时断链；
- Android 嵌入场景必须在构造 endpoint 前安装 Android JNI/DNS context；Termux spike 的 panic
  fallback 不能成为产品路径。

来源：<https://docs.iroh.computer/concepts/relays>、
<https://docs.rs/iroh/1.0.3/iroh/>、
<https://docs.rs/iroh-relay/1.0.3/iroh_relay/server/>。

## 决策

### D-1 两个独立版本边界

`TRANSPORT_VERSION = "0.1.0"` 与 `UACP = "0.3.x"` 不变。公网控制面新增独立闭合版本：

```text
REMOTE_CONTROL_VERSION = "0.1.0"
```

只接受 `0.1.x`。公网 rendezvous、route、push 与 relay admission 的 wire 形状全部归
`REMOTE_CONTROL 0.1`；未知 version、kind、字段、route、request/response ID 一律 fail-closed。
UACP 业务 enum 不为路由或 push 增加变体。

### D-2 数据面是“iroh byte pipe 外层 + R3 TLS 内层”

公网数据面固定为：

```text
mobile ephemeral iroh endpoint
  → custom self-hosted iroh relay / direct IP path
  → persistent Host iroh endpoint
  → one bidirectional QUIC stream
  → unchanged R3 TLS 1.3 + exact Host SPKI pin
  → TRANSPORT 0.1 hello + UACP hello + P-256 device challenge
  → authenticated business frames
```

iroh 负责寻址、打洞、relay 回退和 QUIC 路径迁移；它的 stream 只是一条有界 byte pipe。
内层 TLS 仍是业务端到端边界，继续提供 Host 身份、前向保密、TLS sequence/rekey/replay
保护和设备认证 channel binding。路径从 relay 迁移到直连或反向迁移时，内层 TLS 与
TRANSPORT connection 不重建，因此业务 frame、pending request 和 projection cursor 不会因
路径改变而重放。

Ubuntu 只看到 iroh EndpointId、route admission、时序和有界密文字节；它没有 Host TLS 私钥、
设备 P-256 私钥、TLS exporter、UACP frame、ContentRef 正文或 provider credential。

产品 endpoint 必须使用 `RelayMode::Custom` 且 relay map 只含部署配置中的自有 URL。`Default`、
`Staging`、n0 公共 URL、insecure TLS verifier 和 0-RTT 全部禁止。iroh 固定为 `=1.0.3`；升级
必须重新核对 Android、relay access、path reporting 与 E2EE 边界。

### D-3 三方身份与持久材料

| 主体 | 权威身份 | 持久材料 | 轮换/恢复 |
|---|---|---|---|
| Host | 既有 `HostId + Host TLS SPKI pin`；iroh EndpointId 只作公网寻址并被配对凭据固定 | R3 TLS key、iroh Ed25519 secret、route admin token | 任一身份 key 轮换都撤销旧 remote grant 并重新配对；损坏 fail-loud |
| Device | 既有 Android Keystore non-exportable P-256 key + `DeviceId` | P-256 key；route access token/FID 仅作为 Keystore 加密 vault 中的 opaque credential | P-256 key 丢失即重新配对；FID 更新覆盖旧值；durable revoke 使全部路径失效 |
| Ubuntu | service TLS SPKI pin；部署证书/DNS 只提供可达性，pin 才是控制面授权身份 | service TLS key、route registry、FCM ADC credential | 双 pin 滚动窗口只可由已配对 Host 明确下发；损坏/权限放宽 fail-loud |

mobile iroh EndpointId 是每次远程 worker 生命周期的临时传输身份，不是 Device 身份，不进入
Actor 或授权。真正的设备授权仍由内层 P-256 challenge 完成。

Host 首次启用 remote 时生成 16-byte random `RouteId`、32-byte route admin token；每个设备
另生成 16-byte random `DeviceSlotId` 与 32-byte access token。Ubuntu 只持久化 token 的
SHA-256 digest。`DeviceSlotId` 不是 `DeviceId`，服务没有二者映射。remote pairing bootstrap
经现有敏感 QR/URI 路径传递 route、slot、access token、Host EndpointId、自有 relay URL、
service endpoint/pin；它与一次性 LAN pairing secret 一样不得进入日志或 analytics。

### D-4 rendezvous、presence 与重放

REMOTE_CONTROL 0.1 使用 TLS 1.3 + exact service SPKI pin；每帧为 4-byte big-endian length 加
`1..=4,096` bytes 的严格 UTF-8 JSON，随后交换 `RemoteHello/Ack`。
每条连接的 request ID 从 1 严格单调，响应逐字回显。每个鉴权请求还带 16-byte random
`operation_id` 与 `issued_at_ms`；服务只接受与当前时间相差不超过 60 秒的请求，并在
120 秒有界 replay cache 内拒绝同一 credential digest + operation ID。cache 满时拒绝新请求，
不得淘汰仍在窗口内的项后接受重放。

Host 以 admin token 注册/刷新 `{ RouteId, Host EndpointId, relay URL }`。presence TTL 可请求
`15..=90` 秒，默认 30 秒；host 每 10 秒刷新。TTL 到期后 device resolve 统一返回
`RouteUnavailable`，不能区分不存在、过期或被撤销。route/grant 是 durable，presence 是
可过期内存状态；服务重启后必须由 host 重新刷新才能 online。

Host 首次 `RegisterRoute` 原子创建无 presence 的 durable route，使自有 relay 能先鉴权固定的 Host
EndpointId；Host 等待 iroh endpoint 的自有 relay 握手完成后才发送 `RegisterPresence`。后续错误
admin token、Host EndpointId 或 relay URL 必须拒绝；随后以 `RegisterDeviceGrant` 为明确的本地设备
登记随机 slot/access-token digest，服务不持有
DeviceId↔slot 映射。Host 用 `WakeDevice` 指定 slot 和随机 wake ID；服务只在有效 grant/address
存在时通过 FCM HTTP v1 发送白名单 data payload，404/UNREGISTERED 必须删除 address。

Device resolve 以 route + slot + access token 鉴权，只返回配对时已经 pin 的 Host EndpointId、
自有 relay URL 与 presence expiry。返回值不覆盖客户端 pin；任一不一致按 service compromise
处理并拒绝连接。

### D-5 relay admission 与限额

host 和 device 都只向 Ubuntu 发起出站连接。iroh relay 的 bearer token 使用 route admin/access
token 的 canonical 编码，`AccessControl` 只比较 digest并恒时验证：host EndpointId 必须等于
当前 route registration；device endpoint 可临时变化，但仍需有效 device grant，随后必须通过
内层 P-256 challenge。

默认硬上限：全局 1,024 条 relay client connection；每 route 8 条；每 device slot 2 条；
每来源 4 条 pre-auth；单连接入站 1 MiB/s、burst 256 KiB；单个 relay protocol frame 使用
iroh 1.0.3 的固定上限；30 秒无合法 relay activity 探测、90 秒 idle 断链。超过任何上限只返回
闭合 `RateLimited` / `LimitExceeded` 或直接关闭尚未安全协商的连接，不返回自由文本详情。

relay 只转发 iroh 加密 packet，不能构造或解析内层 TLS/UACP frame。服务日志字段白名单为：
事件 kind、闭合错误码、route/slot 的进程内短期哈希、连接/字节计数、持续时间、时间戳；不得记录
token/digest、FID、EndpointId 全值、完整 endpoint/path、Host/Device canonical ID 或请求 body。

### D-6 push 是平台无关 opaque address

共享合同使用：

```text
PushProvider = FcmFid | ApnsToken
PushAddress { provider, opaque_address, registered_at_ms, expires_at_ms }
```

R4 只实现 `FcmFid`；`ApnsToken` 形状先闭合但实机接入留 R8。根据 2026-07 Firebase 当前合同，
Android 使用 Messaging 25.1.1 / BoM 34.16.0 的 `onRegistered(FID)` 与 HTTP v1 `message.fid`；
deprecated registration token API 不进入产品代码。FID 是敏感 credential，服务持久化时加密或
owner-only，普通 Debug/tracing 一律 `[redacted]`。

FCM 只发送 data-only payload：

```json
{"v":"1","kind":"wake","route":"<22-char opaque route hint>","wake":"<22-char random id>"}
```

四个 key 固定且不允许扩展，UTF-8 JSON 最大 256 bytes。payload 不含正文、ContentRef、cursor、
HostId、DeviceId、endpoint、错误详情或 token。wake 只触发 Android WorkManager/worker 从
last-good per-key cursor 重连；它本身不代表新状态或命令成功。FCM 使用 HTTP v1 + ADC 短期
OAuth token；service account 文件不得由命令行参数或日志回显。

FID 注册、更新、删除都需有效 device grant；过期默认 30 天，App 每次启动或 FID 回调刷新时间。
FCM 返回目标无效/404 后删除该 address。device revoke 事务先删除 access grant 和 push address
并 fsync，再 ack 和断开当前 relay endpoint。

### D-7 路径与恢复状态机

精确顺序：

1. 保留所有 `ProjectionKey` 的 last-good cache/cursor，不因连接尝试清空；
2. 有 LAN endpoint 时先尝试 LAN；只有完成 pin、两次 hello、设备认证和订阅屏障后才发布
   `LanDirect`；
3. LAN 失败后向已 pin 的 service resolve；presence 过期则进入带本地观测时间的 `Offline`；
4. 用配对时 pin 的 Host EndpointId 和仅自有 relay 的 custom map 建立 iroh connection，打开
   bidirectional stream，并在其上完成内层 R3 TLS/认证；
5. 认证完成且 iroh selected path 是 IP 才发布 `PeerToPeer`，selected relay 才发布 `Relayed`；
   connecting/candidate path 不得写 online；
6. iroh path watcher只在新 selected path 已可用后切换 reachability；内层连接保持不变；
7. QUIC/inner TLS/session expiry/网络切换导致连接断开时停止新请求，保留 last-good cache，建立
   新连接、重新 challenge，并逐 key 以 last-good cursor 订阅；
8. `CurrentFollows` 仍按 TRANSPORT §6 的 Ping/Pong 屏障验证，不能无条件覆盖 cache；
9. host/runtime 真离线时只显示 Offline；Ubuntu 不缓存业务 frame、不代理 Agent、不回滚已进入
   canonical durable write 的命令，也不重发未提交命令。

### D-8 durable revoke、存储与隐私

Host revoke 顺序固定为：本机 device registry fsync → remote revoke outbox fsync → Ubuntu
grant/push 删除 fsync → Ubuntu ack → 断开该 slot 的 relay endpoint → 关闭全部内层连接。
若 Ubuntu 不可达，本机认证已经永久拒绝，pending remote revoke 持久重试；host 不再为该 slot
发 wake。若进程在本机 registry fsync 后、outbox fsync 前崩溃，重启必须以本地 revoked 集合和
持久 DeviceId→slot 映射幂等补写缺失 outbox，并在发布 presence 前冲刷；运行期每次维护也先
重试全部 pending revoke。重启不能短暂重新授权。

Host/service identity、route registry、FCM credential 使用 owner-only 父目录/文件、同目录临时
文件、file fsync、原子 rename、目录 fsync；Windows 使用当前用户+SYSTEM protected DACL。
缺失只允许首次生成，损坏或权限放宽 fail-loud，不能静默换 key/route。

R4 安全诊断只允许路径类别 `<SANDBOX>`、`<HOME>`、`<PATH>`；先匹配更具体的 canonical
sandbox，再匹配 home，永远不附相对路径、用户名或 basename。这在产品 remote 日志边界结清
D-B8，不修改冻结 recorder。

### D-9 进程树终止

Windows runtime 放入受控进程树并使用整树终止语义；Linux/macOS 在 spawn 时建立独立 process
group，终止时向该 group 发信号并等待根进程。三平台 CI 都运行真实 root + live child 测试，
子进程持有继承 pipe，只有两者都退出才出现 EOF。只 kill 根 PID 的变异必须超时变红。

## 闭合错误域

REMOTE_CONTROL 0.1 只有：

```text
VersionMismatch | MalformedFrame | AuthenticationFailed | RouteUnavailable
Expired | Replay | RateLimited | LimitExceeded | Revoked | Internal
```

错误 frame 只有 `request_id: Option<u64>`、code、retriable；无 detail/string/source。pairing、
route、grant、push 是否存在不得由错误码或外部时序区分。

## 拒绝的方案

- 公共 iroh relay：没有生产隔离/可用性保证，且违反自有 Ubuntu 门禁；
- 自建明文 TCP/WebSocket relay：需要重做打洞、身份和 E2EE，且更容易让 relay 读取业务；
- 在 Ubuntu 终止内层 TLS：服务器将取得正文与命令，违反 OBJ-9；
- 只依赖 iroh E2EE 并移除 Host pin/设备 challenge：会扩大认证语义并失去 R3 已验证的 revoke；
- 把 FID、cursor 或 projection key 放入 push：扩大第三方可见元数据，wake 不需要这些字段；
- 把 path connecting 当 online，或切换时清空 cache：制造假在线或 cursor 回退。

## 后果

- 新增 `kaleido-relay` 与 `REMOTE_CONTROL 0.1` Rust 类型；
- hostd/core 需要把既有 blocking TLS wire 抽象到 TCP 或 iroh bidirectional byte pipe；
- Android 只负责 FCM FID/lifecycle、Keystore signer 与 secure vault，路由/cursor 状态机仍在 Rust；
- 未提供真实 Ubuntu、DNS/防火墙、Firebase 项目与实体 arm64 蜂窝网络时，代码/自动化可以完成，
  但 T-110 真实门禁必须保持未通过，不能用 loopback/emulator/mock 替代。
