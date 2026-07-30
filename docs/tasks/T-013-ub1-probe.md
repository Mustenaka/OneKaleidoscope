# T-013: UB-1 探针 —— Codex Desktop 是否连接共享 daemon

> 执行人：**项目负责人本人**，在自己的交互式 PowerShell 里
> 不下发 Codex（需真实 Codex + Desktop + 网络）
> 预计耗时：10 分钟
> 决定：[REQUIREMENTS §11](../REQUIREMENTS.md) 的 **UB-1** 能否销号

---

## 这个探针决定什么

若 Codex Desktop 连接的是共享 daemon，则 hostd 能订阅 Desktop 正在进行的 turn ——
`observe_external_live` / `control_external_live` 对 Codex 成立，
**负责人的完整需求（GUI 实时显示在手机上）可达**，[ADR-0009](../adr/0009-session-broker.md)
的 Broker 架构在 Codex 上完整落地。

若 Desktop 用私有实例，则 Broker 只覆盖 CLI，UB-1 保留登记。

---

## 已确认的事实

```
codex app-server daemon
  bootstrap               Install durable local app-server management for SSH-driven use
  start / restart / stop
  enable-remote-control   Enable remote control for future starts and a currently running managed daemon
  disable-remote-control
  version                 Print local CLI and running app-server versions as JSON

control socket 固定路径：~/.codex/app-server-control/app-server-control.sock
```

`codex app-server proxy` 此前失败是因为 **daemon 未启动**（socket 不存在，os error 10050），
不是路径或权限问题。

`thread/loaded/list` 的响应类型（0.146.0 快照）：

```json
ThreadLoadedListResponse {
  "data": ["...thread ids..."],   // "Thread ids for sessions currently loaded in memory"
  "nextCursor": string | null
}
```

`ThreadLoadedListParams` 无必填字段，`{}` 即可。

---

## 第 A 步：最小配置先试（**不要**先开 remote-control）

先测最简配置。如果这样就通了，我们连厂商的 remote-control 都不需要碰。

### A-1 起 daemon 并确认

```powershell
codex app-server daemon start
codex app-server daemon version
```

`version` 应输出 JSON，含本地 CLI 与**正在运行的** app-server 版本。
若它报「未运行」，说明 `start` 没成功，把输出发给主管。

### A-2 打开 Codex Desktop，建一个会话

- 打开 Codex Desktop
- **新建一个会话**，发一句好认的话，例如：`KALEIDO UB1 PROBE DESKTOP`
- 等它回复完
- **把 Desktop 保持打开**，不要关

### A-3 用 proxy 问 daemon 看到了什么

先写探针报文：

```powershell
@'
{"id":1,"method":"initialize","params":{"capabilities":{},"clientInfo":{"name":"kaleido-ub1-probe","title":"OneKaleidoscope UB-1 probe","version":"0.1.0"}}}
{"method":"initialized"}
{"id":2,"method":"thread/loaded/list","params":{}}
'@ | Set-Content -Encoding utf8 ub1-probe.jsonl
```

再喂给 proxy。**`Start-Sleep` 是必要的** —— 直接管道会在响应回来前就关掉 stdin：

```powershell
& { Get-Content ub1-probe.jsonl; Start-Sleep -Seconds 8 } | codex app-server proxy
```

### A-4 判读

看 `id:2` 那条响应的 `data` 数组：

| 结果 | 含义 |
|---|---|
| `data` 非空 | 有内存中的会话。**进 A-5 确认它就是 Desktop 那个** |
| `data` 为空 `[]` | daemon 看不到 Desktop 的会话 → 进第 B 步 |

### A-5 确认那个 thread 就是 Desktop 的

拿 `data[0]` 的 thread id，问它的内容：

```powershell
@'
{"id":1,"method":"initialize","params":{"capabilities":{},"clientInfo":{"name":"kaleido-ub1-probe","title":"OneKaleidoscope UB-1 probe","version":"0.1.0"}}}
{"method":"initialized"}
{"id":3,"method":"thread/read","params":{"threadId":"<把 data[0] 填这里>"}}
'@ | Set-Content -Encoding utf8 ub1-read.jsonl

& { Get-Content ub1-read.jsonl; Start-Sleep -Seconds 8 } | codex app-server proxy
```

**如果返回内容里有 `KALEIDO UB1 PROBE DESKTOP` 这句话 —— UB-1 销号，需求可达。**

---

## 第 B 步：仅在 A 步 `data` 为空时做

```powershell
codex app-server daemon enable-remote-control
```

然后**重启 Desktop**（让它重新连接），重做 A-2 ~ A-5。

> **注意**：这一步可能触发 Codex 自己的 remote-control 配对流程。
> [ADR-0009](../adr/0009-session-broker.md) D-5 已决定**不把厂商 remote-control 作为技术路线**，
> 这里开它只是为了测「Desktop 会不会因此连上共享 daemon」。
> **如果它要求你登录、配对某个手机 App，或提示内容会经过 OpenAI 服务器 —— 停下来告诉主管**，
> 那意味着这条路撞 OBJ-3，需要重新裁决。

---

## 要交回给主管的东西

1. `codex app-server daemon version` 的 JSON 输出
2. `thread/loaded/list` 的完整响应（`data` 数组）
3. 若 `data` 非空：`thread/read` 的响应里**有没有**你在 Desktop 里说的那句话
4. 若走了 B 步：`enable-remote-control` 的输出，以及它**有没有**要求登录/配对

**不要贴几百行 candidate 清单**，只要上面四项。

---

## 三种结果对应的处置

| 结果 | 处置 |
|---|---|
| A 步就看到 Desktop 的 thread | **最好的结果。** UB-1 销号，Broker 在 Codex 上完整成立，连 remote-control 都不用碰 |
| 需要 `enable-remote-control` 且它是纯本地的 | UB-1 销号，但「一次性启用集成」的具体动作就是这条命令，写进产品的首次配置引导 |
| 需要 `enable-remote-control` 且它要求厂商配对 | **UB-1 保留，且撞 OBJ-3。** 主管重新签 ADR，Codex 的 GUI 那半降级为 `resume_persisted` |
