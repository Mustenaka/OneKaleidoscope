# ADR-0023：projection 初始同步使用有序 Ping/Pong 屏障

- 状态：**已接受，2026-08-10**
- 决策人：Codex（用户授权的项目主管角色）
- 任务：[T-108](../tasks/T-108.md)
- 补充：[ADR-0020](0020-projection-cursors-and-mobile-ingress.md)、
  [ADR-0022](0022-projection-subscription-terminal-frame.md)

## 背景

订阅合同固定为先发送 `ProjectionSubscribeAckFrame`，再发送首次 current/replay envelopes。
当 `Resumed` 的 `since == head` 时不会发送 envelope。若移动端把“收到 ack”直接当成同步完成，
非空 replay 可能尚未到达；若只等 projection callback，完全同步且没有新事件的重连又会永久停留在
`CachedOffline`。两种做法都不能同时证明安全与可用。

新增 subscribe ack 或 terminal frame 会扩充闭合 wire enum；在 Kotlin 中用定时器猜测 replay 已结束也
没有协议证据。因此需要一个不改变 TRANSPORT/UACP shape 的有序完成证据。

## 决策

### D-1 使用既有 Ping/Pong 作为连接内顺序屏障

客户端收到并验证非 `Rejected` 的 subscribe ack 后，立即在同一 TLS 连接发送一个新的、单调递增
request ID 的 `Ping { request_id, nonce = request_id }`，并等待严格匹配的 `Pong`。

hostd 对一条连接串行处理 control frame。它必须先完成该 subscribe request 的 ack 与全部 initial
current/replay envelope 写入，之后才会读取这个 Ping 并写回 Pong。因此客户端在等待 Pong 时会先
验证、应用并原子缓存所有 initial envelopes。Pong 到达即证明：

- `CurrentFollows` 的指定 current envelope 已完整应用；
- `Resumed` 的保留窗口已完整应用；
- `since == head` 的零-envelope resume 已追平该次订阅捕获的 head。

屏障只证明初始同步边界；其后的 live entries 继续按各 projection cursor 顺序处理。
hostd 的循环可在读取 Ping 前先发布已经排队的 live entry，因此 Pong 时 cache cursor 可以大于
`CurrentFollows.current_cursor`。core 必须接受这个经过严格连续校验的前进，只拒绝 cache cursor
小于 ack/current 或小于原 resume anchor；不得用相等断言把合法 live 竞态当成合同错误。

### D-2 core 返回语义

`MobileClient::subscribe` 只有在匹配 Pong 到达且目标 key 的 cache 存在、cursor 与 ack 语义一致后
才返回成功。Android 可在成功返回后读取该 key 的 core cache并标为 `Live`；不得在 subscribe 返回
前、连接成功时，或仅凭 provider/版本推断 freshness。

错误 nonce/request ID、缺失 Pong、目标 cache 缺失、CurrentFollows cursor 不一致或传输中断均
fail closed：订阅失败、UI 保留 last-good cache并显示离线，不提升能力按钮。

### D-3 安全与版本

Ping/Pong 只携带连接内数值 request ID/nonce，不携带正文、cursor、key、路径或设备凭据。该决策
复用 TRANSPORT 0.1 已有闭合 frame和 request correlation，不改变 wire shape，因此不提升
`TRANSPORT_VERSION`。

## 拒绝的方案

- 让服务端在 envelope 前发送 ack 之后立即把 UI 标成 Live：存在旧 projection 回滚窗口；
- `since == head` 时伪造重复 envelope：违反“不重发 head”和严格 cursor 规则；
- 把 ack 移到 replay 之后：违反 ADR-0020 已固定的订阅顺序；
- 新增 `ProjectionSynchronized` frame：能表达，但为现有有序 request/response 已可证明的事实扩大
  兼容面；
- 定时等待“看起来没有更多 envelope”：网络时序不是合同证据。

## 验证

- 首次 CurrentFollows 的 callback被阻塞时，`subscribe` 必须仍未返回；
- 移除 Ping/Pong 等待后，上述测试必须变红；
- exact-cursor cold resume 不重复 callback，但 `subscribe` 成功返回且 cache cursor 不变；
- Android 接受 core cache 时推进 per-key high-water，旧排队 callback 不得回滚视图，等值 callback
  只负责把 freshness 提升为 `Live`。
