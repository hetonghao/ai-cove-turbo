# Turbo 下行 zstd 可行性研究

日期：2026-08-06

## 结论

- **Hybrid WS 与纯私有 WS 的公网下行已经使用 zstd，无需再设计一套 WS 压缩。** New API 对发往 Turbo 的应用消息按 `ai-cove-zstd.v1` 编码，Turbo 解码后再向本地 Codex 发送普通 WebSocket 消息；该私有协议双向生效、逐消息自适应，仅在压缩结果更小时使用 zstd（`new-api/controller/responses_websocket_transport.go:100-118`、`turbo/src-tauri/src/proxy/private_websocket/codec.rs:75-127`、`turbo/src-tauri/src/proxy/private_websocket/relay.rs:138-183`）。下一步应先补下行 raw/wire 指标，确认真实命中率和收益。
- **仍有优化空间的是 HTTP/SSE 下行，包括 Turbo HTTP、Hybrid 首轮 HTTP 和回退 HTTP。** HTTP 可以用标准 `Content-Encoding: zstd`，但当前源码与边缘模板没有专门的 SSE zstd 实现；运行态是否已有 Compression Rule 本次未核验。不能只添加响应头，必须先保证 Turbo 能流式解压，并验证每个 SSE 事件仍可及时读取。[RFC 9110 Content-Encoding](https://www.rfc-editor.org/rfc/rfc9110.html#section-8.4)、[Accept-Encoding](https://www.rfc-editor.org/rfc/rfc9110.html#section-12.5.3)
- **优先做 Cloudflare Compression Rule 小流量验证，不直接改 New API。** Cloudflare 可向访客发送 zstd，但到源站的 `Accept-Encoding` 只使用 `br, gzip`；其默认可压缩 MIME 类型也不含 `text/event-stream`。因此必须用 Compression Rule 显式试验 SSE，并把“首字不退化、事件不聚合”设为硬门槛。[Cloudflare Compression](https://developers.cloudflare.com/speed/optimization/content/compression/)、[Compression Rules](https://developers.cloudflare.com/rules/compression-rules/)
- **预期收益主要是公网下行字节与流量成本，不是模型速度。** 正常宽带下，Responses 输出通常由模型生成节奏而非下行带宽主导；只有大工具结果、长 JSON 或弱网场景更可能缩短总耗时。没有真实 raw/wire 与事件时间数据前，不应把它计入 Turbo 的提速百分比。

## 当前链路矩阵

| Turbo 路径 | 上游到 Turbo 的响应 | Turbo 到 Codex | 当前下行 zstd 状态 | 判断 |
|---|---|---|---|---|
| Turbo HTTP | HTTP/SSE，经 Cloudflare、Nginx、New API | 透明流式 HTTP/SSE | 未见专用 SSE zstd；运行态规则未核验 | 先补 Turbo 解码 fixture，再做 Cloudflare 小流量试验 |
| Hybrid 首轮/回退 HTTP | HTTP/SSE，由 Hybrid worker 解析 | 本地 WS 普通消息 | 未实现；解析器当前要求明文 SSE | 必须先具备流式解压，不能直接开规则 |
| Hybrid WS | 私有 WS `ai-cove-zstd.v1` | 本地 WS 普通消息 | 已实现双向自适应 zstd | 只补指标，不新增协议 |
| 纯私有 WS | 私有 WS `ai-cove-zstd.v1` | 本地 WS 普通消息 | 已实现双向自适应 zstd | 只补指标，不新增协议 |

Turbo HTTP 当前把上游 `bytes_stream()` 和响应头直接转交客户端；`reqwest` 依赖也未启用 `zstd` feature（`turbo/src-tauri/src/proxy.rs:518-607`、`turbo/src-tauri/Cargo.toml:23`）。Hybrid HTTP 则把响应 body 直接送入 `SseParser`，压缩字节若未先流式解压会破坏解析（`turbo/src-tauri/src/proxy/hybrid_http.rs:81-105`）。

New API `/v1` relay 仅安装请求体解压中间件，没有响应 gzip/zstd 中间件（`new-api/router/relay-router.go:13-18,77-118`、`new-api/middleware/gzip.go:26-96`）；全局 gzip 当前处于注释状态（`new-api/main.go:188-189`）。Node1/Node2 的流式 Nginx 路径均为 `proxy_buffering off`、`gzip off`（`deploy/nodes/node1/nginx/new-api-edge.conf.example:54-57,82-85`、`deploy/nodes/node2/nginx/api.ai-cove.com.conf.example:69-72`）。

标准 WebSocket 压缩只有 RFC 7692 的 `permessage-deflate`，没有标准 `permessage-zstd`。[RFC 7692](https://www.rfc-editor.org/rfc/rfc7692.html) 当前私有协议不使用 RSV 位，也不与 `permessage-deflate` 叠加，避免二次压缩（`new-api/docs/protocols/ai-cove-zstd-v1.md:3-28,40`）。

## 收益与风险边界

- 对重复度较高的 SSE JSON，**下行字节减少 30%–70% 只能作为试验预算估算**，不能作为现状或承诺；必须由 raw/wire 实测替换。
- 字节减少不等于 token 减少，也不等于模型推理、首 token 或总耗时同比改善。流式 zstd 如果等待更大块或未及时 flush，反而可能增加 TTFT；SSE 必须保持逐事件可见。[WHATWG SSE](https://html.spec.whatwg.org/multipage/server-sent-events.html)、[zstd streaming API](https://facebook.github.io/zstd/zstd_manual.html)
- 小响应、已压缩或不可压数据可能不划算，应保留 identity，并坚持“压后严格更小”才启用。
- 解压必须限制窗口、声明长度和总输出，防止内存/CPU 放大；私有 WS 已有 128 MiB 消息上限及长度校验。HTTP 实现也应采用有界流式解压。[RFC 9659](https://www.rfc-editor.org/rfc/rfc9659.html)、[RFC 8878 安全考虑](https://www.rfc-editor.org/rfc/rfc8878.html#section-8)
- 认证上下文中的动态秘密与攻击者可控内容若位于同一可观测压缩体，存在压缩侧信道风险；不要跨请求复用压缩上下文。[RFC 9110 §17.6](https://www.rfc-editor.org/rfc/rfc9110.html#section-17.6)

## 最小实施与验证顺序

1. **先观测，不改协议：** 为 HTTP、Hybrid WS、纯私有 WS 记录下行 raw bytes、wire bytes、解压 bytes、压缩命中率和失败原因；按路径、首轮/续轮、小/中/大响应分组。
2. **验证现有 WS：** 检查 `ai-cove-zstd.v1` 的下行命中率、字节缩减、解压 CPU、TTFT、总耗时、Close/解码错误；不新增 `permessage-zstd`。
3. **先锁定 Turbo 接收语义：** 用本地分块 zstd fixture 覆盖 Turbo HTTP 与 Hybrid HTTP，证明首个 SSE 事件可在压缩流结束前读到、事件边界不变、`Content-Encoding`/`Content-Length` 被正确处理，且损坏/超限流会明确失败。`reqwest` 提供 zstd 自动解压选项，但仍须验证它在本链路上的分块行为与响应头改写。[reqwest `ClientBuilder::zstd`](https://docs.rs/reqwest/0.12.28/reqwest/struct.ClientBuilder.html#method.zstd)
4. **再做 Cloudflare 小流量试验：** 仅对测试标记或极小流量的 `/v1/responses` SSE 使用 Compression Rule；同时采集 Turbo 实际收到的 `Content-Encoding`、每个事件到达时间、TTFT、总耗时、CPU、断流率和 identity 对照。任一事件聚合、首字显著退化或兼容失败即回退。
5. **最后才考虑源站 writer：** Cloudflare 官方只向源站协商 `br, gzip`，因此不要在现有公开源站路径盲加 zstd writer。只有确认 Cloudflare 可透明承载该编码，或另有受控直连入口时，才考虑 New API 的 SSE 流式 zstd writer；必须逐事件 flush，并保留 identity 协商与快速回退。

验收矩阵至少覆盖：三条路径（Turbo HTTP、Hybrid HTTP、私有 WS）× identity/zstd × 小/中/大响应 × 正常完成/错误/取消/断流；核心指标为实际编码、raw/wire 比、首字、总耗时、CPU、事件间隔、内存上限和失败率。只有在 **TTFT 不退化、流式语义不变、错误率不升高** 后，才能扩大 HTTP/SSE zstd 流量。
