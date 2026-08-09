# ADR-0022：projection lag 使用独立订阅终止 frame

- 状态：**已接受，2026-08-09**
- 决策人：Codex（用户授权的项目主管角色）
- 任务：[T-107](../tasks/T-107.md)
- 补充：[ADR-0020](0020-projection-cursors-and-mobile-ingress.md)、
  [ADR-0021](0021-r3-lan-security.md)

## 背景

TRANSPORT 0.1 要求慢订阅者落后 bounded channel 时收到 `CursorGap`，且只关闭发生 lag 的
subscription；同一 TLS 连接上的其他订阅和请求应继续工作。但是原闭合 control frame 集只有
首次 `ProjectionSubscribeAckFrame` 和普通 projection push：

- 再发一次 subscribe ack 会复用 request ID，违反 correlation 规则；
- `TransportErrorCode` 没有 `CursorGap`，且 canonical 业务错误不得伪装成 transport 错误；
- 直接关闭 TLS 虽然 fail-closed，却会无故中断其他健康订阅，也无法向客户端给出明确恢复原因。

因此原合同无法同时实现闭合 frame、明确 `CursorGap` 和“只关闭一个订阅”。

## 决策

### D-1 独立 terminal push

认证后 control enum 新增唯一的订阅终止变体：

```text
ProjectionSubscriptionClosed {
  subscription_id: u64,
  error: CanonicalError,
}
```

它是 server push，不含也不复用 `request_id`。`subscription_id` 必须对应这条连接上的一个
active subscription。TRANSPORT 0.1 中该 frame 只允许：

```text
error.code       == CursorGap
error.retriable  == true
error.detail_ref == None
```

`at_ms` 是 Broker 检测到 lag 的时间。错误仍是 UACP `CanonicalError`，不是 transport error；
因此不扩充 `TransportErrorCode`，也不泄漏自由文本详情。

### D-2 有序关闭与恢复

Broker 的单一订阅 actor 按以下顺序处理 lag：

1. 停止为该 subscription 排队新的 projection；
2. 从 active 集移除并保留本连接内 tombstone；
3. 若连接仍可安全写入，发送一条 `ProjectionSubscriptionClosed`；
4. 释放该 subscription 的 channel 与 journal tail；
5. 保留 TLS 连接和其他 active subscription。

客户端收到 terminal frame 后保留最后一个已经完整验证并原子落盘的 envelope/cursor；后续重新
订阅时以该 cursor 作为 `since`。客户端不得应用 gap 之后的任何 envelope，也不得自行递增 cursor。

若 terminal frame 无法写出，连接可按既有 IO 规则关闭；重连仍从 last-good cursor 恢复。
同一 subscription 的 terminal frame 重复、未知 ID、terminal 后继续 push 都是
`MalformedFrame` 并关闭连接。

### D-3 与主动 unsubscribe 的竞态

Broker actor 的处理顺序决定唯一结果：

- `UnsubscribeRequest` 先到：返回 `UnsubscribeAck`，不再发送 terminal frame；
- lag 先到：发送 terminal frame；已经在途的 `UnsubscribeRequest` 对同一 tombstone 返回一次
  `UnsubscribeAck`，但不得复活或复用该 subscription ID。

这样不会因为正常竞态关闭整条健康连接，同时仍保持 ID 单调且不可复用。

## 拒绝的方案

- 复用原 subscribe request ID 再发 ack：破坏 request correlation；
- 把 `CursorGap` 塞进 `TransportError`：混淆 canonical 与 transport 错误域；
- 仅关闭 TLS：扩大故障域并丢失明确恢复证据；
- 静默丢 projection 或跳 cursor：制造假恢复。

## 后果

- `kaleido-transport` 的闭合 control enum 与 correlation 状态机新增一个 server-push 变体；
- hostd subscription hub 必须保留本连接内 terminal tombstone；
- `kaleido-core` 收到该 frame后保留 last-good cache并重新订阅；
- T-107 必须测试 lag 只关闭目标订阅、其他订阅继续、重复 terminal/terminal 后 push 失败，以及
  unsubscribe 竞态不复活 ID。
