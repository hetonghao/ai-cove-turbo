# Turbo hybrid HTTP 正常结束被记为 client_gone（2026-09-17）

## 1. 本次成功案例的事实

链路：Codex WS → Turbo（本机私有 WS 代理）→ New API（HTTP POST /v1/responses）→ 渠道 62（`www.string.ink`，普通上游中转站）。

会话 `new-test`（本机时间 11:44–11:56，用户 2 / 令牌 Codex / 分组 API）：

| Turbo 侧（`com.aicove.turbo` traffic.jsonl） | 值 |
|---|---|
| 请求数 | 10（`hybridCapabilityHttp`，全部 HTTP） |
| 结果 | 10/10 `status=200`、`result=success` |

| New API 侧（`logs`） | 值 |
|---|---|
| 计费消费行 | 8 行有 usage，金额正常 |
| 零用量行 | 3 行（`上游没有返回计费信息，无法扣费`，quota=0） |
| 上游错误行 | 1 行 `status_code=500, upstream error: do request failed` |
| 前端红色感叹号 | 8 行，字段为 `stream_status = {"end_reason":"client_gone","end_error":"context canceled","status":"error"}` |

结论：**任务本身成功，感叹号不是任务失败**。它是 New API 侧对"流结束原因"的记账结果。

## 2. 感叹号的确切含义

- 前端 `web/src/features/usage-logs/components/timing-metrics-cell.tsx`
  显示条件：`isStream && stream_status && stream_status.status !== 'ok'`，图标 `CircleAlert`，tooltip 展示 `end_reason`。
- 后端 `service/log_info_generate.go::appendStreamStatus`
  规则：`status = "ok"`，当 `!IsNormalEnd() || HasErrors()` 时为 `"error"`。
- `IsNormalEnd()`（`relay/common/stream_status.go`）只认 `done`、`eof`、`handler_stop`。
- 次要影响：`web/src/features/usage-logs/lib/format.ts::isTurboWarmupLog` 也依赖 `stream_status.status !== 'error'`，误报会连带影响 Turbo 预热行的识别。

## 3. 根因

`client_gone` 的写入点：`relay/helper/stream_scanner.go` 主循环

```go
case <-c.Request.Context().Done():
    // 客户端断开：立即 cleanup 关闭上游 resp.Body，解除 scanner 阻塞并让上游停止生成
    info.StreamStatus.SetEndReason(relaycommon.StreamEndReasonClientGone, c.Request.Context().Err())
```

`StreamStatus.SetEndReason` 使用 `sync.Once`，**第一个原因生效**。

Turbo 侧：`src-tauri/src/proxy/hybrid_http.rs::run_http_worker` 收到终态事件（`response.completed` / `response.done`）后立即 `control.complete(); return;`，函数返回即丢弃响应体，HTTP 连接关闭。

因此正常回合的时序是：

1. 上游发出 `response.completed`；
2. Gateway 转发该事件给 Turbo，但仍在读取上游剩余字节；
3. Turbo 判定完成并关闭连接；
4. Gateway 的请求 ctx 被取消 → 记为 `client_gone` → 前端显示感叹号。

证据：

- Turbo 记录的 10 条请求全部 200 / success。其中 4 条与 Gateway 记为 `client_gone` 的行一一对应（11:44:07、11:49:40、11:53:18、11:55:03）：客户端认为成功、Gateway 认为被取消，误报成立。
- 另外 4 条 `client_gone` 行（11:49:03、11:49:04、11:49:38、11:54:56）在 Turbo 的 traffic 记录里不存在，属于 Turbo 未记账的取消（Codex 打断或会话回收）。其中前 3 条没有 usage，是真实取消；11:54:56 有 usage，机制上与误报同类。
- Gateway 日志：`stream ended: reason=client_gone end_error="context canceled", received=19`，伴随 `scanner error: read tcp ...: use of closed network connection`（cleanup 关闭上游 body 所致）。
- 未取消的行落到 `reason=eof`，`status=ok`：说明 Responses SSE 没有 `[DONE]`，Gateway 的 `done` 只由 `[DONE]` 触发。

另一类同字段但语义不同的行：11:49:03 / 11:49:04 / 11:49:38 三行在**终态事件之前**被取消（`received=7`），Gateway 读不到 usage，只能写"上游没有返回计费信息，无法扣费"。这类是真实取消或上游 500 后重试被放弃，不是误报。

## 4. 修复方案

### 方案 A（推荐）：Gateway 在 Responses 终态事件标记正常结束

位置：`relay/channel/openai/relay_responses.go`，`response.completed` / `response.done` 分支。

```go
case "response.completed", "response.done":
+   // Responses SSE 没有 [DONE]，终态事件就是正常结束；先占住 end reason，
+   // 之后客户端断开不会再覆盖成 client_gone。
+   info.StreamStatus.SetEndReason(relaycommon.StreamEndReasonDone, nil)
    if streamResponse.Response != nil {
```

- 效果：`end_reason=done`、`status=ok`、感叹号消失；`sync.Once` 保证后续 `client_gone` 不能覆盖。
- 不改读取行为、不改计费路径，只修记账；对正常完成的回合生效，真实中途取消仍报 `client_gone`。
- 备选变体：调用 `sr.Done()`（`relay/helper/stream_result.go`）会在该 chunk 后停止 scanner，省掉等待上游关闭的时间，但也可能截断尾随事件；仅在确认上游无尾随 usage 事件后使用。

### 方案 B（可选，客户端）：终态事件后有界 drain

`hybrid_http.rs` 在终态事件后继续读取响应体到 EOF 或超时（建议 ≤1s）再返回。可从源头消除 `client_gone`，但给每个请求增加尾部延迟；方案 A 落地后不必做。

### 不做的事

不要把 `client_gone` 一律当正常。真正的"终态事件之前取消"必须保留告警，否则会掩盖中途取消与上游 500 叠加的问题。

## 5. 与前一版 Turbo 优化设计的关系

前一版设计（本地拦截空 input 帧与 HTTP 续传帧，让 Codex 重发完整上下文）**不变**，仍是消除错误 A / 错误 B 的手段。本次新增的结论是分工修正：

- "状态形态不可用"的判断与拒绝，属于 Turbo（客户端掌握完整历史）。
- "流以什么原因结束"的记账，属于 Gateway（只有它同时看到上游流与下游断开）。
- 两者不能互相替代：Gateway 修好记账不会让错误 A / 错误 B 消失，Turbo 拦截也不会让感叹号消失。

## 6. 验收方式（修复后）

1. 同类会话跑一轮，New API `logs.other.stream_status` 应为 `{"end_reason":"done","status":"ok"}`。
2. 日志页对应行不应再出现红色感叹号。
3. 人为中断一轮（Codex 停止生成），该行应仍为 `client_gone` / `status=error`。
4. 上游 500 与零用量行仍按原样暴露，不因本次改动被隐藏。
