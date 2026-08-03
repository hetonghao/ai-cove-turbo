# Turbo 三路径基准

## 覆盖范围

基准使用同一个 AI Cove Responses 请求，按以下顺序分别测量：

1. 直连 `https://api.ai-cove.com/v1/responses`，不经过 Turbo。
2. 经过 Turbo 的 HTTP POST，Turbo 公网腿使用 HTTP zstd。
3. 经过 Turbo 的 WebSocket，Turbo 公网腿使用 `ai-cove-zstd.v1`。

每个场景默认预热 1 次、正式采样 5 次，输出每个样本以及 median/P95。

## 计时口径

- `E2E`：从发起请求到完整响应结束。HTTP 读取完整响应体；WebSocket 读取到 `response.completed` 或 `response.done`。
- `传输`：请求体交给 HTTP 发送器或 WebSocket 应用帧发送完成。HTTP 使用请求体流已交给客户端发送器的边界，不宣称内核 socket flush；它不包含模型推理和响应下载。
- `WS 握手`：仅 WebSocket 场景单列，包含在该场景的 E2E 内，但不计入 `传输` 列。
- `raw → wire`：只统计请求应用负载，不包含 HTTP/WS 头、TLS、TCP/IP 头。HTTP zstd 数值来自 Turbo 实际指标；WS 数值来自 Turbo 私有 envelope 实际指标。

因此这不是网卡级抓包计时；`传输` 是客户端发送完成边界，适合比较三条请求路径的传输开销，不能单独推导完整对话提速。

## 执行

在 `turbo/` 目录执行：

```bash
export AI_COVE_API_KEY  # 由安全环境注入，不要把值写进命令历史或仓库
TURBO_BENCHMARK_RUNS=5 TURBO_BENCHMARK_WARMUPS=1 npm run benchmark:live
```

测试只读取 `AI_COVE_API_KEY`，不会修改用户的 `~/.codex/config.toml`。可选环境变量：

```text
TURBO_BENCHMARK_UPSTREAM       默认 https://api.ai-cove.com/v1
TURBO_BENCHMARK_MODEL          默认 gpt-5.6-luna
TURBO_BENCHMARK_PROMPT         默认固定的 256 行可压缩上下文；自定义时应使用较大、重复度高的输入
TURBO_BENCHMARK_RUNS           默认 5，必须大于 0
TURBO_BENCHMARK_WARMUPS        默认 1，可为 0
TURBO_BENCHMARK_TIMEOUT_SECS   默认 180，必须大于 0
```

输出中的 `相对直连` 是 median 比值：`直连 median / 当前场景 median`；大于 `1x` 表示当前场景更快。真实公网测试应保留每次原始样本，不要只看单次结果。

## 2026-08-04 脱敏实测

参数为默认上游、默认模型、预热 1 次、正式 5 次；请求输入为默认的 256 行重复上下文。执行命令中的 API Key 只通过环境变量注入，输出未包含凭证、请求正文或响应正文。

```text
场景                 E2E median/P95 ms  传输 median/P95 ms  WS 握手 median/P95 ms  raw → wire       节省
直连（不走 Turbo）    1817.6/2750.2      0.4/0.6             0.0/0.0                20040 → 20040    0.0%
HTTP POST + zstd      3823.1/6033.4      0.2/0.3             0.0/0.0                20040 → 138       99.3%
WS + zstd             3037.3/3506.6      0.0/0.1             962.6/1208.7           20051 → 154      99.2%
```

相对直连的 median 比值为：HTTP E2E `0.48x`、传输边界 `1.78x`；WS E2E `0.60x`、传输边界 `7.38x`。其中传输边界只有亚毫秒量级，受本机调度和计时分辨率影响，不能当成公网线路的真实 RTT；本次最可靠的结论是请求字节节省，E2E 需要在更多轮次、固定模型负载和连接复用策略下继续采样。
