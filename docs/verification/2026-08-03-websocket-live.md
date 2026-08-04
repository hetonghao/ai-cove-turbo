# 2026-08-03 AI Cove WebSocket 线上验收

## 范围与证据边界

- 上游：`https://api.ai-cove.com/v1`。
- 验证的是 Turbo 到 AI Cove 的真实认证 Upgrade，以及隔离配置的真实 Codex 请求是否被 Turbo 观测为 WebSocket。
- API Key 只由环境变量注入；测试输出不打印凭证、Authorization、请求正文或响应正文。
- 这不是对用户真实 `~/.codex/config.toml` 的修改：Codex 测试使用 `tempfile` 创建隔离 `CODEX_HOME`，标准输出和错误输出均丢弃。
- 握手用例只证明 WebSocket 已通：生产 `101` 未返回 `Sec-WebSocket-Extensions`，因此不证明 `permessage-deflate` 已协商。独立的真实 Codex 用例另行断言 Turbo 私有 WS zstd metrics，不把公网扩展协商伪装成压缩证据。

## 执行命令

```bash
cargo test --manifest-path src-tauri/Cargo.toml \
  live_ai_cove_websocket_handshake_passes_through_turbo -- --ignored

cargo test --manifest-path src-tauri/Cargo.toml \
  live_codex_request_uses_turbo_websocket -- --ignored
```

## 脱敏结果

```text
test proxy::tests::live_ai_cove_websocket_handshake_passes_through_turbo ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 15 filtered out

test proxy::tests::live_codex_request_uses_turbo_websocket ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 15 filtered out
```

## 可复查断言

- 认证握手测试要求 Turbo 返回 `HTTP/1.1 101`，并要求内存指标记录 `websocket_verified = true`、`websocket_handshakes = 1`。
- Codex 测试要求隔离 `codex exec --ephemeral` 成功退出；提示会要求一次只读 `pwd` 工具往返，以尽量让同一 Codex session 发送多条 Responses 应用消息。Turbo metrics 记录 `websocket_handshakes`、`websocket_messages` 和 `websocket_failures`，测试输出只保留这些计数及派生的 `reconnects = handshakes - 1`。
- 当真实 Codex 只建立一次握手时，测试要求 `messages_per_connection >= 2`；若观察到多次握手，测试会报告每次重连证据，并且不把聚合消息数伪装成单连接复用。
- 本次复查实际输出：`handshakes=1, messages=3, messages_per_connection=3, reconnects=0, failures=0`；测试同时确认私有 WS zstd 编码负载小于原始应用负载。
- 测试实现：`src-tauri/src/proxy.rs` 中两个 `#[ignore]` 线上用例；普通 `npm test` 不会自动触发外部生产请求。
