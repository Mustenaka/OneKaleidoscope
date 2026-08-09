# ADR-0020：Projection 独立游标与可信移动端入口

- 状态：**已接受，2026-08-09**
- 决策人：用户（项目主管）
- 任务：[T-106](../tasks/T-106.md)
- 取代：D-B3 的“跨 stream projection cursor 不阻塞”判断

## 背景

UACP 0.2 把 `ProjectionEnvelope.cursor` 直接取自其名义 `StreamKey`。这在 R2 的一次性读
模型里足够，但不能作为手机重连 token：

- `SessionIndex` 名义属于 Project stream，Session/Turn/Queue 变化却写 Session stream；
- `AttentionInbox` 名义属于 Host stream，session attention 写 Session stream；
- Transcript、LiveActivity、InputQueue 共用一个 Session stream，`Subscribe` 无法选择
  到底订阅哪个 projection。

结果是 projection 内容已经变化而 cursor 不动，或同一 cursor 对应多个不同读模型。若直接
上线，手机会静默漏状态。

另有两个 ingress 缺口：手机需要写入 prompt/free-form 后才能得到 `ContentRef`；同时不能
允许远端自行构造 `Actor::Broker`、command ID 或时间。

## 决策

### D-1 UACP 提升至 0.3

UACP `0.3.0` 是 pre-1.0 minor 边界，只接受 `0.3.x`。`0.2.x` peer 在任何业务解码前
拒绝。此次不猜测迁移旧 durable log；本 ADR 不改变现有 `StateEffect` 的持久形状。

### D-2 ProjectionKey 是唯一游标域

新增闭合 `ProjectionKey`：

- ProjectIndex / Host；
- SessionIndex / Project；
- Transcript、LiveActivity、InputQueue / Session；
- AttentionInbox / Host；
- WorkflowBoard / Workflow；
- RuntimeCapability / Host + Runtime。

`ProjectionEnvelope` 从 `stream` 改为 `key`。cursor 只在这个 key 内有意义，由持久
projection journal 严格 `+1` 分配；canonical stream head 不再冒充 projection cursor。

projection 不是增量 patch，而是该 key 在该 cursor 下的完整读模型。canonical append 后按
显式 fanout matrix 重算可能受影响的 key；只有 payload 逐字段改变才追加新 journal entry。

### D-3 Mobile 订阅只传 projection

新增 `ProjectionSubscribe`、`ProjectionSubscribeAck` 与闭合 outcome：

- `Resumed { from_cursor }`：保留窗内从 `since + 1` 开始；
- `CurrentFollows { current_cursor }`：首次订阅或 since 早于 floor，随后发送当前完整
  `ProjectionEnvelope`；
- `Rejected { error }`：ahead、key 无权访问、版本或其他业务错误。

Android 不接收 canonical `SnapshotEnvelope` / `LogRecord` 并重写 reducer。Rust mobile
core 只维护 projection journal cursor/cache，Kotlin 只接收完整 projection callback。

若 live channel 背压导致无法保序，服务端关闭该订阅并返回 CursorGap；客户端用最后成功
cursor 重连，不能静默跳到当前值。

### D-4 设备身份进入可信 Actor

新增 canonical `DeviceId`。`Actor::Human` 只携带由配对目录确认的 `device_id`；可读设备名
属于 transport 元数据，不参与授权或幂等域。

手机只提交 `DeviceCommandRequest { idempotency_key, ttl_ms, body }`。hostd 必须从已认证
连接注入 DeviceId、issued_at、expires_at 和 command ID，再构造 `CommandEnvelope`。
远端没有表达 Broker/Workflow actor 或自定 command ID/时间的入口。

幂等身份是 `(device_id, idempotency_key)`。服务端持久化规范化请求摘要：同摘要重试返回
Duplicate；不同摘要复用同 key 返回 IdempotencyConflict。命令接收与 outbox 必须具备崩溃
恢复语义，不能因 side table / log 写入窗口而盲目重发 runtime。

### D-5 ContentWrite 是独立鉴权操作

新增 ContentWrite request/response，但它不是 Command、StateEffect、durable log 或
projection。R3 只允许最大 64 KiB 的 PlainText/Markdown bytes；host 忽略客户端对
sensitivity/preview 的任何暗示，强制 Sensitive、无 preview，并自行计算 digest、ContentId
和 ContentRef。

孤儿上传必须有 TTL/配额。ContentWrite/Read 和后续 command 都绑定同一已认证 DeviceId；
正文只存在于端到端加密 transport body 与 content store，不进入普通 tracing。

## 拒绝的方案

### A. 用 Project/Host stream head 继续当 projection cursor

拒绝。子 stream 变化不会推进它，已经有可复现的漏更新路径。

### B. Android 接收 canonical log 并自行派生 UI

拒绝。Rust core 虽可复用 reducer，但会把多流合并、snapshot 应用和完整 canonical state
复制到移动端，扩大安全面，也偏离“移动端消费 projection”的既定边界。

### C. 远端直接发送 CommandEnvelope

拒绝。它允许伪造 actor、命令时间和 Broker/Workflow 身份。

### D. 把 prompt bytes 塞进 Command

拒绝。会旁路 ContentRef 的敏感正文与保留策略。

## 后果

- projection 重连 token 与真实可见变化一一对应；
- Android 客户端不实现 canonical reducer；
- state 层必须增加持久 projection journal 与 fanout matrix；
- UACP handshake 提升为 0.3，Kotlin/Swift 生成代码需要同步编译；
- 设备认证和正文传输仍由独立 TRANSPORT 合同定义。
