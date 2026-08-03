# 2026-08-03 AI Cove WebSocket 线上验收

## 范围与证据边界

- 上游：`https://api.ai-cove.com/v1`。
- 验证的是 Turbo 到 AI Cove 的真实认证 Upgrade，以及隔离配置的真实 Codex 请求是否被 Turbo 观测为 WebSocket。
- API Key 只由环境变量注入；测试输出不打印凭证、Authorization、请求正文或响应正文。
- 这不是对用户真实 `~/.codex/config.toml` 的修改：Codex 测试使用 `tempfile` 创建隔离 `CODEX_HOME`，标准输出和错误输出均丢弃。
- 本次只证明 WebSocket 已通。生产 `101` 未返回 `Sec-WebSocket-Extensions`，因此不证明 `permessage-deflate` 已协商，也不证明私有 WS zstd 已实现。

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
- Codex 测试要求隔离 `codex exec --ephemeral` 成功退出，并要求 Turbo 记录至少一次 WebSocket 握手。
- 测试实现：`src-tauri/src/proxy.rs` 中两个 `#[ignore]` 线上用例；普通 `npm test` 不会自动触发外部生产请求。
