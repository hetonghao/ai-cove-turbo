# Turbo 3×4 基准测试（历史，已废弃）

> **归档说明：** 本文的 3×4 模型性能结论已被公平连接生命周期的
> [3×3 基准](./2026-08-04-three-by-three-fair-benchmark.md)取代。纯 WS、不压缩仅保留兼容性测试；本文以下数据只用于追溯历史，不再用于产品路径判断。

## 目标

这套基准用于反复比较 Turbo 优化前后的真实效果，固定覆盖 3 类负载 × 4 条技术路径。

### 使用场景

| 场景 | 行为 | 逻辑请求数 |
|---|---|---:|
| 单轮短上下文 | 固定短输入，观察小包压缩决策及 WS envelope 开销 | 1 |
| 单轮长上下文 | 默认约 96 KiB 的源码型输入，或指定的脱敏固定夹具 | 1 |
| 连续 5 轮固定长上下文 | 在同一 WS 连接逐轮发送完整长输入；HTTP 仍为 5 个独立请求 | 5 |

默认长输入取版本控制中的 `src-tauri/Cargo.lock` 前 98,304 个 ASCII 字节/字符，报告标记为 `builtin-source-smoke`。该依赖元数据夹具已获 Issue #16 批准，FNV-1a 为 `6fd028d239956991`；它避免旧版“同一句话重复 256 次”产生不具代表性的 99% 压缩率。未来若比较其他版本，必须同时记录来源、大小和 FNV-1a。

## 2026-08-04 Issue #16 正式 3×4 基线（当前结论）

正式基线已完成：默认 AI Cove upstream、`gpt-5.6-luna`、内置 fixture、`runs=12`、`warmups=1`；四条路径均完成 12 个正式样本，测试结果为 `1 passed; 0 failed`，运行时间为 1152.23 秒。下面的模型表是本文当前唯一的性能结论；后面的三路径内容明确保留为历史预基线。

稳定可复核的结果是：98,304-byte 源码型输入在 Turbo HTTP 和 Turbo WS 上均约减少 78.4% 的请求应用负载；连续五轮 WS 在单条连接上完成 `1 handshake / 5 messages / 0 reconnects`（直连标准 WS 与 Turbo WS 均如此）。模型 E2E、TTFT 和生命周期受公网、服务端排队、模型推理与冷启动波动影响，本次观测不证明 Turbo 的因果提速。

### 正式运行、fixture 与源码状态

| 项目 | 值 |
|---|---|
| 上游 / 模型 | `https://api.ai-cove.com/v1` / `gpt-5.6-luna` |
| 采样 | `runs=12`、`warmups=1`；runs 为 4 的正整数倍，四路轮转位置均衡 |
| fixture | `builtin-source-smoke`；版本控制中的 `src-tauri/Cargo.lock` 前 98,304 个 ASCII 字节/字符 |
| fixture 指纹 | `fixture_bytes=98304`；FNV-1a `6fd028d239956991` |
| 来源文件 SHA-256 | 完整 `src-tauri/Cargo.lock`（132,529 bytes）为 `d9064a13e9bd187ad3e91b4a3ad6833348685392a19dad45dcf738e44a4b8537`；该值是来源 provenance，不是 98,304-byte workload fingerprint |
| Turbo revision | `3c618ac7c69e536ee1354bd2434bd5ed72ebfe9f` |
| 结果 artifact | `/Users/hetonghao/dev/hth-project/ai-cove/.omo/teams/019fc58c-4d9f-7222-b887-0dcca353f72a/artifacts/issue-16-final-baseline.txt` |

正式执行命令（安全环境中的 `AI_COVE_API_KEY` 值从未写入 stdout 或 artifact）：

```bash
cd /Users/hetonghao/dev/hth-project/ai-cove/turbo
set -o pipefail
: "${AI_COVE_API_KEY:?secure AI_COVE_API_KEY is required; do not print it}"
unset TURBO_BENCHMARK_PROMPT TURBO_BENCHMARK_PROMPT_FILE
export TURBO_BENCHMARK_RUNS=12
export TURBO_BENCHMARK_WARMUPS=1
export TURBO_BENCHMARK_MODEL=gpt-5.6-luna
npm run benchmark:live 2>&1 | tee /Users/hetonghao/dev/hth-project/ai-cove/.omo/teams/019fc58c-4d9f-7222-b887-0dcca353f72a/artifacts/issue-16-final-baseline.txt
# npm script invokes:
# cargo test --manifest-path src-tauri/Cargo.toml live_three_by_four_benchmark -- --ignored --nocapture
```

本次捕获时 nested Turbo checkout 有意保留既有 dirty state；A 未回退或覆盖 B、原型及其他已有改动。当前状态由清单和最终 diff gate 复核，涉及：

```text
docs/verification/2026-08-03-websocket-live.md
docs/verification/2026-08-04-three-path-benchmark.md
package.json
prototype/app.js
prototype/index.html
prototype/styles.css
src-tauri/src/benchmark.rs
src-tauri/src/benchmark/live.rs
src-tauri/src/benchmark/live/http.rs
src-tauri/src/benchmark/live/websocket.rs
src-tauri/src/benchmark/live/runner.rs
src-tauri/src/benchmark/report.rs
src-tauri/src/benchmark/report/
src-tauri/src/benchmark/settings.rs
src-tauri/src/benchmark/tests.rs
src-tauri/src/lib.rs
src-tauri/src/proxy.rs
src-tauri/src/proxy/private_websocket.rs
src-tauri/src/proxy/private_websocket/codec.rs
src-tauri/src/proxy/private_websocket/relay.rs
src-tauri/src/transport_ack_benchmark.rs
src-tauri/src/transport_ack_benchmark/
```

### 模型客户端：E2E、TTFT 与 WS 生命周期

下表所有时长均为毫秒，格式为 `median[min,max]`。`complete` 是读完完整响应；HTTP 的 TTFT 是首个有效 JSON SSE 事件，WS 的 TTFT 是首个 Text/Binary 应用数据。`请求/消息/事件/握手` 中的事件是服务端流式响应事件，不是额外的用户请求。

| 场景 | 路径 | 总 E2E complete | TTFT | 每轮 complete | cold setup | warm request | connection lifetime | reconnects | messages/connection | 请求/消息/事件/握手 |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| 单轮短上下文 | 直连 HTTP | `1672.8[1382.6,2144.8]` | `1027.8[828.7,1775.0]` | `1672.8[1382.6,2144.8]` | — | — | — | — | — | `1/1/9/0` |
| 单轮短上下文 | Turbo HTTP + zstd | `1752.7[1308.9,4512.9]` | `1147.8[862.6,4323.5]` | `1752.7[1308.9,4512.9]` | — | — | — | — | — | `1/1/9/0` |
| 单轮短上下文 | 直连 WS（标准 WebSocket） | `3561.2[3044.1,5910.6]` | `879.7[774.6,1134.3]` | `1569.5[1292.7,1989.9]` | `1771.2[1626.3,4399.5]` | — | `1569.6[1292.8,1989.9]` | `0` | `1` | `1/1/11/1` |
| 单轮短上下文 | Turbo WS + zstd | `3746.2[3050.7,6748.7]` | `867.3[766.9,1092.4]` | `1614.2[1315.0,2892.4]` | `1919.0[1631.1,5156.8]` | — | `1614.3[1315.0,2892.4]` | `0` | `1` | `1/1/11/1` |
| 单轮长上下文 | 直连 HTTP | `2338.0[1665.0,3075.5]` | `1505.0[1216.4,2832.5]` | `2338.0[1665.0,3075.5]` | — | — | — | — | — | `1/1/11/0` |
| 单轮长上下文 | Turbo HTTP + zstd | `2437.6[1915.5,6734.4]` | `1603.5[1279.0,4973.7]` | `2437.6[1915.5,6734.3]` | — | — | — | — | — | `1/1/11/0` |
| 单轮长上下文 | 直连 WS（标准 WebSocket） | `5241.2[4134.7,7186.2]` | `1878.0[1700.3,3011.5]` | `2791.3[2422.6,5370.5]` | `1777.0[1639.1,2878.9]` | — | `2791.4[2422.7,5370.9]` | `0` | `1` | `1/1/11/1` |
| 单轮长上下文 | Turbo WS + zstd | `5056.8[3899.3,8489.7]` | `1408.1[1251.7,3332.3]` | `3154.3[2180.4,6214.1]` | `2096.6[1659.4,3541.9]` | — | `3154.4[2180.5,6214.2]` | `0` | `1` | `1/1/11/1` |
| 连续 5 轮固定长上下文 | 直连 HTTP | `14539.7[13163.9,17782.6]` | `1736.9[1290.1,4577.2]` | `2823.4[1787.8,5408.8]` | — | — | — | — | — | `5/5/49/0` |
| 连续 5 轮固定长上下文 | Turbo HTTP + zstd | `12596.0[10043.3,16483.0]` | `1384.8[1001.6,4690.6]` | `2302.0[1541.1,5201.1]` | — | — | — | — | — | `5/5/47/0` |
| 连续 5 轮固定长上下文 | 直连 WS（标准 WebSocket） | `15613.8[11582.5,23450.0]` | `1456.1[741.5,5804.5]` | `2580.6[1493.8,6467.5]` | `1950.1[1640.0,4773.3]` | `2532.6[1493.8,6467.5]` | `13422.3[9514.9,20222.0]` | `0` | `5` | `5/5/55/1` |
| 连续 5 轮固定长上下文 | Turbo WS + zstd | `14499.4[10926.8,18925.3]` | `1099.4[719.9,3284.1]` | `2035.1[1257.8,5175.3]` | `2046.9[1659.0,3301.3]` | `1982.7[1257.8,4531.4]` | `11999.9[8710.6,16750.2]` | `0` | `5` | `5/5/57/1` |

原始样本没有被重新汇总或丢弃：artifact 的汇总表在第 15–35 行，12 个 `samples:` 块从第 39、53、67、81、95、109、123、137、151、165、179、193 行开始，全部 `#1..#12` 原始行覆盖第 39–205 行；测试通过尾部为第 206–215 行。这样既保留可审计 raw samples，也避免在报告中复制 168 行噪声。

### 模型请求应用负载与理论值（独立于 E2E）

`原始 → 编码` 只计算请求应用负载：HTTP 为 JSON body，WS 为应用消息与私有 envelope；不含 headers、TLS、TCP/IP、重传或响应流。`payload@10Mbps` 只按应用字节换算理论序列化时间，不是公网单向传输实测。

| 场景 | 路径 | 原始 → 编码 bytes | 减少率 | payload@10Mbps ms |
|---|---|---:|---:|---:|
| 单轮短上下文 | 直连 HTTP | `159 → 159` | `0.0%` | `0.13 → 0.13` |
| 单轮短上下文 | Turbo HTTP + zstd | `159 → 159` | `0.0%` | `0.13 → 0.13` |
| 单轮短上下文 | 直连 WS（标准 WebSocket） | `170 → 170` | `0.0%` | `0.14 → 0.14` |
| 单轮短上下文 | Turbo WS + zstd | `170 → 180` | `-5.9%` | `0.14 → 0.14` |
| 单轮长上下文 | 直连 HTTP | `108255 → 108255` | `0.0%` | `86.60 → 86.60` |
| 单轮长上下文 | Turbo HTTP + zstd | `108255 → 23399` | `78.4%` | `86.60 → 18.72` |
| 单轮长上下文 | 直连 WS（标准 WebSocket） | `108266 → 108266` | `0.0%` | `86.61 → 86.61` |
| 单轮长上下文 | Turbo WS + zstd | `108266 → 23418` | `78.4%` | `86.61 → 18.73` |
| 连续 5 轮固定长上下文 | 直连 HTTP | `541715 → 541715` | `0.0%` | `433.37 → 433.37` |
| 连续 5 轮固定长上下文 | Turbo HTTP + zstd | `541715 → 117275` | `78.4%` | `433.37 → 93.82` |
| 连续 5 轮固定长上下文 | 直连 WS（标准 WebSocket） | `541770 → 541770` | `0.0%` | `433.42 → 433.42` |
| 连续 5 轮固定长上下文 | Turbo WS + zstd | `541770 → 117377` | `78.3%` | `433.42 → 93.90` |

短 WS 包的 `170 → 180` 是标准私有 envelope 的开销，不应被显示为压缩收益。长输入的理论 10 Mbps 序列化节省约 67.9 ms/请求（五轮约 339.5 ms），明显小于本次模型 E2E 的秒级范围；因此不得把该理论值、ACK RTT 或 noisy E2E 差异写成 Turbo 因果提速。

### 本地 Transport ACK（独立证据，不是模型 E2E）

该证据来自隔离 loopback New API `127.0.0.1:3300` 与临时 Turbo proxy，固定 67,257-byte application payload，`1 passed; 0 failed`、运行 0.57 秒。它用于观察客户端 ACK RTT、WS setup 及 New API `receive_ms`/`decode_ms`，不代表公网基线、模型 TTFT 或单向上传时间。

| 路径 | client_ack_rtt_ms | websocket_setup_ms | receive_ms | decode_ms | wire_bytes | decoded_bytes |
|---|---:|---:|---:|---:|---:|---:|
| direct_http | `23.399` | `—` | `0.047` | `0.122` | `67257` | `67257` |
| turbo_http | `8.149` | `—` | `0.006` | `0.078` | `102` | `67257` |
| direct_websocket | `0.447` | `4.189` | `0.415` | `0.000` | `67257` | `67257` |
| turbo_websocket | `3.289` | `2.725` | `0.835` | `0.846` | `112` | `67257` |

完整本地 ACK 输出见 `/Users/hetonghao/dev/hth-project/ai-cove/.omo/teams/019fc58c-4d9f-7222-b887-0dcca353f72a/artifacts/issue-16-ack-local.txt`；四行输出保留 `client_ack_rtt_ms`、`websocket_setup_ms`、`receive_ms`、`decode_ms`、`wire_bytes`、`decoded_bytes`，不包含 payload、token 或完整 headers。执行命令为：

```bash
cd /Users/hetonghao/dev/hth-project/ai-cove/turbo
TURBO_TRANSPORT_ACK_UPSTREAM=http://127.0.0.1:3300/v1 \
  cargo test --manifest-path src-tauri/Cargo.toml live_transport_ack_benchmark -- --ignored --nocapture
```

### 压缩 CPU 与当前策略（Issue #15）

完整数据表和 raw samples 见 `/Users/hetonghao/dev/hth-project/ai-cove/.omo/teams/019fc58c-4d9f-7222-b887-0dcca353f72a/artifacts/issue-15-compression-measurement.md`。该证据只测实际 HTTP/private-WS 编码 worker 成本，不把 New API `receive_ms`/`decode_ms`、模型 latency 或网络 latency 混进来。

| 阈值判断 | saved_serialization_ms（10 Mbps 理论） | worker duration max | 结论 |
|---|---:|---:|---|
| raw 512 HTTP | `0.334` | `0.466 ms` | 未通过保守 worst-case 成本门槛 |
| raw 1024 HTTP | `0.743` | `0.139 ms` | 通过 |
| raw 1024 private WS | `0.735` | `0.079 ms` | 通过 |

当前策略是 `<1024` bytes 保留 raw；`>=1024` bytes 仅在压缩后严格更小时发送压缩结果（private WS 仍包含固定 10-byte AICZ envelope）。这项 CPU/策略证据与模型 E2E、ACK RTT、理论 payload@10Mbps 分开解释。

### 生产 ACK 边界

**BLOCKED：生产 ACK baseline 尚未测量。** 当前没有 leader/user 的生产部署与生产 capture 授权；本次未部署、未改生产配置、未发生产 ACK 请求。后续只有在获得明确授权后，才能按同一四路径合同执行生产证据；在此之前只能报告以上隔离本地 ACK，不得把它标成生产或公网 baseline。

### 技术路径

| 路径 | 真实行为 |
|---|---|
| 直连（不走 Turbo） | HTTP POST 直接请求 AI Cove |
| Turbo HTTP + zstd | HTTP POST 经过 Turbo，公网请求正文按需使用 zstd |
| 直连 WS（标准 WebSocket） | 标准 WebSocket 直接请求 AI Cove；不声明 `permessage-deflate`，不使用 Turbo 私有 subprotocol |
| Turbo WS + zstd | 每个样本建立一次 WS，再经 `ai-cove-zstd.v1` 发送一条或多条 `response.create` |

## 采样设计

- 默认预热 1 次、正式采样 12 次。
- 每轮按 `直连 HTTP→Turbo HTTP→直连 WS→Turbo WS`、随后四路循环左移，平衡固定执行顺序造成的服务端和网络时窗偏差。
- runs 必须是 4 的正整数倍，确保四条路径处于每个执行位置的次数完全相同；正式对比建议使用 12、16 或 24。
- 报告保留每个原始样本，并给出 `median[min,max]`。少量样本不再输出伪 P95。
- 所有路径使用同一模型、同一场景输入、相同最大输出 token；WS cold setup 单独列出但包含在总 E2E 中。
- 多轮 WS 在一个连接上逐条发送 `response.create`；不做隐式连接池或自动重连。

## 指标口径

### 真实测量

- `总 E2E`：该样本从开始到所有完整响应结束。WS 包含一次 setup。
- `TTFT`：HTTP 从请求开始到首个有效 JSON `data:` SSE 事件；WS 从发送本轮应用消息到首个 Text/Binary 应用数据消息。
- `每轮 complete`：单轮发送请求到收到 `response.completed` 或 `response.done`，HTTP 还必须读完响应流。
- `cold setup`：建立 WS 并完成握手的时间；HTTP/非 WS 路径显示 `—`。
- `warm request`：同一 WS 连接上从第二轮开始的 complete 时间；单轮场景显示 `—`。
- `connection lifetime`：WS 握手完成到显式 close 完成的持续时间；非 WS 路径显示 `—`。
- `reconnects`：显式建立的新 WS 连接次数；当前实现不重连，异常断开直接失败并保留证据边界。
- `messages/connection`：一条 WS 连接承载的应用消息数，不把服务端响应事件当成请求数。
- `原始正文 → 编码负载`：HTTP 为 JSON body 压缩前后字节；WS 为原始应用消息与私有 envelope 字节。正减少率表示变小，负数表示编码后增大。

字节指标不包含 HTTP/WS framing、headers、TLS、TCP/IP、重传和响应流量，因此不能称为完整网络流量。

### 理论值

- `payload@10Mbps`：按 `bytes × 8 ÷ 10 Mbps` 计算的请求应用负载序列化时间。
- 它只用于把确定性字节变化换算为统一带宽下的量级，不包含 RTT、拥塞、协议开销、响应和模型推理，也不冒充公网实测。

### 当前不测

现有 Responses 接口没有“服务端收完请求正文立即 ACK”的时间戳，因此客户端无法准确分离公网上传、服务端排队与模型处理。旧版 reqwest body poll 和本地 WS `send()` 时间只代表本机交付边界，已经删除。

Responses 本身仍没有“服务端收完请求正文立即 ACK”的边界；本次正式报告因此把模型基准与独立的本地 Transport ACK 套件分开，不能把 ACK RTT 当成模型 E2E 或公网单向上传时间。

## 执行

在 `turbo/` 目录执行：

```bash
export AI_COVE_API_KEY  # 只由安全环境注入，不要打印
unset TURBO_BENCHMARK_PROMPT TURBO_BENCHMARK_PROMPT_FILE
TURBO_BENCHMARK_RUNS=12 TURBO_BENCHMARK_WARMUPS=1 TURBO_BENCHMARK_MODEL=gpt-5.6-luna \
  npm run benchmark:live 2>&1 | tee /absolute/path/to/issue-16-final-baseline.txt
```

正式对比建议固定脱敏夹具：

```bash
export TURBO_BENCHMARK_PROMPT_FILE=/absolute/path/to/sanitized-codex-context.txt
TURBO_BENCHMARK_RUNS=16 TURBO_BENCHMARK_WARMUPS=1 npm run benchmark:live
```

快速流程验证可执行：

```bash
TURBO_BENCHMARK_RUNS=4 TURBO_BENCHMARK_WARMUPS=0 npm run benchmark:live
```

可选环境变量：

```text
TURBO_BENCHMARK_UPSTREAM       默认 https://api.ai-cove.com/v1
TURBO_BENCHMARK_MODEL          默认 gpt-5.6-luna
TURBO_BENCHMARK_PROMPT         内联长输入；不能与 PROMPT_FILE 同时设置
TURBO_BENCHMARK_PROMPT_FILE    UTF-8 脱敏固定夹具路径
TURBO_BENCHMARK_RUNS           默认 12，必须是 4 的正整数倍
TURBO_BENCHMARK_WARMUPS        默认 1，可为 0
TURBO_BENCHMARK_TIMEOUT_SECS   默认 180，必须大于 0
```

基准不会修改 `~/.codex/config.toml`，也不会输出 API Key、请求正文或响应正文；指标只保留时长、计数和字节总量。

## 比较规则

两次结果只有同时满足以下条件才可比较：

1. 上游、模型、负载指纹、runs、warmups 相同。
2. 四条路径均完成，长输入的 HTTP/WS 编码负载都小于原始正文。
3. 同一路径每次样本的原始/编码字节一致，否则基准直接失败。
4. 字节和理论序列化时间用于评估传输优化；E2E 只报告观测差异，不把模型与服务端波动归因于 Turbo。
5. 小 WS 包若编码后变大，减少率必须显示为负数，不能截断为 0%。

## 已废弃结果

2026-08-04 旧表中的 `20040 → 138`、约 99% 节省来自极端重复合成正文；“传输耗时”也没有覆盖公网发送。该表只保留在 Git 历史中用于说明基准缺陷，不再作为产品、性能或流量结论。

## 2026-08-04 三路径预基线（历史预基线 / 已被本次 3×4 正式基线取代）

本节保留四路径改造前的三路径预基线，仅用于核对既有指标修正；它没有 TTFT 或完整生命周期字段，已被上面的正式 3×4 基线取代，不得作为当前性能结论。新的可复现实验口径与当前结论见上文正式 3×4 段。

### 运行信息

| 项目 | 值 |
|---|---|
| 执行时间 | 2026-08-04 12:36 CST |
| 命令 | `TURBO_BENCHMARK_RUNS=3 TURBO_BENCHMARK_WARMUPS=0 npm run benchmark:live`（旧 3×3 代码基点；当前实现要求 4 的倍数） |
| 上游 / 模型 | `https://api.ai-cove.com/v1` / `gpt-5.6-luna` |
| 负载 | `builtin-source-smoke`，98304 bytes，FNV-1a `6fd028d239956991` |
| 客户端 | macOS 26.5.2，Apple M1 Pro，arm64 |
| 工具链 | Rust/Cargo 1.97.1，Node.js 25.9.0，npm 11.12.1 |
| 代码基点 | `a2cd0a2e4aba04002d6f8554b8595f26fc02bc3d` + 当前未提交基准修正 |
| 完成情况 | 63 个真实逻辑请求全部成功；测试 1 passed；190.16 秒 |

这是用于核对指标和发现缺口的 3 轮预基线，不是发布用正式性能结论。三种执行顺序各覆盖一次，但样本量仍不足以抵消公网、服务端排队、模型推理和提示缓存波动。

基准客户端 gate 会先对真实上游直连 WS 发送连续 5 条 `response.create`，要求一次握手、5 条应用消息、0 次重连；它只证明 benchmark client 的多轮连接生命周期，不是“真实 Codex”验收标准。失败时直接报错，不把每请求重连隐藏成连接复用。

真实 Codex 的验收标准是独立的 ignored 测试
`proxy::tests::live_codex_request_uses_turbo_websocket`：它启动隔离的 `codex exec --ephemeral`，使用只读 `pwd` 工具往返，并从 Turbo metrics 输出实际握手、应用消息、每连接消息数和重连证据。本次复查结果为 `handshakes=1, messages=3, messages_per_connection=3, reconnects=0, failures=0`；这条结果不由上面的 5 轮 benchmark-client gate 代替。

### 历史预基线当时的指标覆盖（已由正式 3×4 更新）

| 指标 | 类型 | 当前状态 |
|---|---|---|
| 总 E2E | 实测 | 从场景开始到完整响应结束；WS 包含 setup |
| 每轮 E2E | 实测 | HTTP 等待完整响应 body；WS 等待 `response.completed` / `response.done` |
| WS setup | 实测 | 本地连接 Turbo 并完成 Turbo 到 AI Cove 上游握手 |
| 请求、应用消息、响应事件、握手数 | 实测 | 用于证明 WS 响应事件没有被误算成用户请求 |
| 原始正文、编码应用负载 | 实测 | 来自 Turbo 运行指标；不含协议头及响应流量 |
| payload@10Mbps | 理论换算 | 仅按应用负载字节换算，不是公网传输实测 |
| TTFT | 实测 | HTTP 首个有效 JSON SSE 事件；WS 首个 Text/Binary 应用数据消息 |
| complete | 实测 | HTTP 读完整 SSE 流；WS 等待 `response.completed` / `response.done` |
| cold setup / warm request | 实测 | WS 分离握手 setup 与同连接第二轮起的请求完成时间；非 WS 为 `—` |
| connection lifetime | 实测 | 握手完成到显式关闭；非 WS 为 `—` |
| reconnects / messages-per-connection | 实测 | 每样本显式计数；多轮 WS 预期为 `0` / `5` |
| 压缩 CPU | 未测 | 当前没有围绕实际 HTTP/WS 编码器单独计时 |
| 公网纯传输阶段 | 未测 | Responses 没有“完整收包立即 ACK”边界 |

### E2E、每轮与握手结果

所有时间单位均为毫秒，格式为 `median[min,max]`。单轮场景的“每轮 E2E”等于去除极小外层开销后的该轮 E2E；5 轮场景的“每轮 E2E”汇总 3 个样本中的 15 轮。

| 使用场景 | 技术路径 | 总 E2E | 每轮 E2E | WS setup | 相对同场景直连总 E2E |
|---|---|---:|---:|---:|---:|
| 单轮短上下文 | 直连 | `1835.0[1427.6,2243.4]` | `1835.0[1427.6,2243.4]` | `0` | 基线 |
| 单轮短上下文 | Turbo HTTP + zstd | `2274.7[1406.2,3452.8]` | `2274.7[1406.1,3452.8]` | `0` | `+24.0%` |
| 单轮短上下文 | Turbo WS + zstd | `3221.4[3093.6,3455.8]` | `1331.3[1274.7,1346.2]` | `1890.2[1818.9,2109.6]` | `+75.6%` |
| 单轮长上下文 | 直连 | `4156.2[2090.4,6165.9]` | `4156.2[2090.4,6165.8]` | `0` | 基线 |
| 单轮长上下文 | Turbo HTTP + zstd | `3506.8[1727.2,3619.7]` | `3506.8[1727.1,3619.6]` | `0` | `-15.6%` |
| 单轮长上下文 | Turbo WS + zstd | `6371.6[5932.2,6880.3]` | `4572.2[3841.0,5034.6]` | `1845.7[1799.4,2091.2]` | `+53.3%` |
| 连续 5 轮固定长上下文 | 直连 | `13158.0[12254.9,15538.7]` | `2423.1[2083.9,4306.8]` | `0` | 基线 |
| 连续 5 轮固定长上下文 | Turbo HTTP + zstd | `14217.1[13534.6,18142.6]` | `2765.3[1702.4,5868.4]` | `0` | `+8.0%` |
| 连续 5 轮固定长上下文 | Turbo WS + zstd | `12481.4[11517.2,15884.1]` | `2092.5[1449.8,4367.4]` | `1794.3[1657.4,2970.6]` | `-5.1%` |

正百分比表示本轮观测用时更长，负百分比表示更短；它不是 Turbo 的因果提速结论。多个路径的范围明显重叠，单轮长上下文直连自身也从 2.09 秒波动到 6.17 秒。

### 请求字节与统一带宽换算

| 使用场景 | 技术路径 | 原始正文 → 编码负载 | 减少率 | payload@10Mbps | 理论节省 |
|---|---|---:|---:|---:|---:|
| 单轮短上下文 | 直连 | `159 → 159` | `0.0%` | `0.13 → 0.13 ms` | `0.00 ms` |
| 单轮短上下文 | Turbo HTTP + zstd | `159 → 129` | `18.9%` | `0.13 → 0.10 ms` | `0.02 ms` |
| 单轮短上下文 | Turbo WS + zstd | `170 → 145` | `14.7%` | `0.14 → 0.12 ms` | `0.02 ms` |
| 单轮长上下文 | 直连 | `108255 → 108255` | `0.0%` | `86.60 → 86.60 ms` | `0.00 ms` |
| 单轮长上下文 | Turbo HTTP + zstd | `108255 → 23399` | `78.4%` | `86.60 → 18.72 ms` | `67.88 ms` |
| 单轮长上下文 | Turbo WS + zstd | `108266 → 23418` | `78.4%` | `86.61 → 18.73 ms` | `67.88 ms` |
| 连续 5 轮固定长上下文 | 直连 | `541715 → 541715` | `0.0%` | `433.37 → 433.37 ms` | `0.00 ms` |
| 连续 5 轮固定长上下文 | Turbo HTTP + zstd | `541715 → 117275` | `78.4%` | `433.37 → 93.82 ms` | `339.55 ms` |
| 连续 5 轮固定长上下文 | Turbo WS + zstd | `541770 → 117377` | `78.3%` | `433.42 → 93.90 ms` | `339.51 ms` |

稳定结论只有应用负载字节：源码型长输入在 HTTP 和 WS 两条 Turbo 路径均减少约 78.4%。在 10 Mbps 上行下，这相当于每个长请求理论少约 67.9 毫秒序列化时间；该量级远小于本次 2–6 秒的完整响应波动，因此不能期待 E2E 与压缩率同比下降。

### 请求、消息、事件和握手

| 使用场景 | 技术路径 | 每样本逻辑请求 / 应用消息 / 握手 | 三个样本的响应事件数 |
|---|---|---:|---:|
| 单轮短上下文 | 直连 | `1 / 1 / 0` | `9, 9, 9` |
| 单轮短上下文 | Turbo HTTP + zstd | `1 / 1 / 0` | `9, 9, 9` |
| 单轮短上下文 | Turbo WS + zstd | `1 / 1 / 1` | `11, 11, 11` |
| 单轮长上下文 | 直连 | `1 / 1 / 0` | `9, 9, 9` |
| 单轮长上下文 | Turbo HTTP + zstd | `1 / 1 / 0` | `9, 11, 9` |
| 单轮长上下文 | Turbo WS + zstd | `1 / 1 / 1` | `11, 11, 11` |
| 连续 5 轮固定长上下文 | 直连 | `5 / 5 / 0` | `47, 45, 49` |
| 连续 5 轮固定长上下文 | Turbo HTTP + zstd | `5 / 5 / 0` | `49, 51, 47` |
| 连续 5 轮固定长上下文 | Turbo WS + zstd | `5 / 5 / 1` | `55, 55, 55` |

WS 的 11 或 55 个响应事件是服务端流式事件，不是把一个请求拆成了 11 或 55 个用户请求。单轮场景仍只有 1 条 `response.create`，5 轮场景为同一连接上的 5 条 `response.create`。

### 原始总 E2E 样本

| 使用场景 | 技术路径 | 样本 1 / 2 / 3 ms |
|---|---|---:|
| 单轮短上下文 | 直连 | `2243.4 / 1835.0 / 1427.6` |
| 单轮短上下文 | Turbo HTTP + zstd | `2274.7 / 1406.2 / 3452.8` |
| 单轮短上下文 | Turbo WS + zstd | `3221.4 / 3455.8 / 3093.6` |
| 单轮长上下文 | 直连 | `4156.2 / 2090.4 / 6165.9` |
| 单轮长上下文 | Turbo HTTP + zstd | `3506.8 / 3619.7 / 1727.2` |
| 单轮长上下文 | Turbo WS + zstd | `6371.6 / 6880.3 / 5932.2` |
| 连续 5 轮固定长上下文 | 直连 | `13158.0 / 12254.9 / 15538.7` |
| 连续 5 轮固定长上下文 | Turbo HTTP + zstd | `13534.6 / 18142.6 / 14217.1` |
| 连续 5 轮固定长上下文 | Turbo WS + zstd | `11517.2 / 15884.1 / 12481.4` |

## 三路径预基线当时尚未覆盖（历史差距；本次正式 3×4 已补齐）

按后续优化评估的价值排序，当前还缺：

1. **压缩 CPU**：围绕 Turbo 实际 HTTP zstd 和 WS envelope 编码器计时，报告每请求 median/range，并区分短包跳过压缩。
2. **公网纯传输实测**：在 AI Cove 相同入口增加鉴权的“完整收包立即 ACK”测试端点，隔离上传、模型排队和推理。
3. **固定真实夹具**：当前 `Cargo.lock` 只适合作为源码型 smoke。正式基线应提交一份脱敏且版本固定的 Codex 请求上下文夹具。
4. **响应侧流量**：当前只统计请求应用负载，未覆盖 SSE/WS 响应、协议头、TLS 和 TCP/IP 字节。
5. **正式样本量**：指标补齐并冻结后，使用固定夹具运行至少 `runs=12, warmups=1`；发布对比不能继续使用本节 3 轮预基线。

暂不增加更多场景或复杂统计。先补齐上述测量边界，再决定是否需要受控带宽/RTT、长输出响应或 20 轮持久连接场景。
