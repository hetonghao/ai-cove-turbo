# AI Cove Turbo 桌面发布

## 构建平台

`.github/workflows/desktop-release.yml` 使用原生 runner：

- `macos-14`：macOS arm64
- `windows-latest`：Windows x64

每个平台先执行 `npm run check` 和 `npm test`，再执行 Tauri 构建。平台产物上传后，在 Ubuntu 汇总 job 中合并并校验 `latest.json`；Windows 同时收集安装器 `.exe` 和 updater 实际消费的 `.exe.zip` 及签名。

## 签名 Secrets

工作流只读取以下 GitHub Actions Secrets，不生成、不写入仓库：

- `AI_COVE_TURBO_TAURI_SIGNING_PRIVATE_KEY`
- `AI_COVE_TURBO_TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

并读取以下 GitHub Actions Repository Variable：

- `AI_COVE_TURBO_TAURI_SIGNING_PUBLIC_KEY`

签名私钥仍需由仓库管理员在 GitHub 仓库 Secrets 中配置；对应独立公钥放入 Repository Variable，构建时注入 `TURBO_UPDATER_PUBLIC_KEY`。工作流通过 `TAURI_CONFIG` 只在正式构建启用 updater artifacts；本地开发构建不要求私钥，并在未注入公钥时显示“未配置”。

## 正式下载地址

合并后的 updater manifest 使用：

```text
https://ai-cove.com/downloads/turbo/latest.json
```

其中每个平台的 updater URL 固定为同目录下的文件名；安装包和签名文件也应同步到：

```text
https://ai-cove.com/downloads/turbo/
```

GitHub Actions 会生成 `ai-cove-turbo-downloads` 汇总 artifact，并在 tag 发布时附加 GitHub Release。将该目录同步到正式静态下载目录的动作需要由现有 AI Cove 发布链路承接。

## 当前客户端接入状态

- `src-tauri/tauri.conf.json` 已配置正式 updater endpoint、平台图标和安装目标。
- `tauri-plugin-updater` 已注册，前端可检查、下载并安装签名更新；未注入独立公钥的本地构建会明确显示“未配置”。
- `src-tauri/Cargo.toml` 与 `src-tauri/tauri.conf.json` 当前版本均为 `0.1.0`。
- 正式构建仍必须提供上述私钥、密码和公钥；缺少任一签名参数时工作流应直接失败。

## WebSocket 发布验证

普通测试使用本地模拟上游验证 Upgrade、标准透明 `permessage-deflate` 头透传、私有 `ai-cove-zstd.v1` 双向 text/binary、固定跨语言解码向量、私有握手剥离 Extensions、拒绝私有能力后的透明重连、连接关闭、禁用与 HTTP zstd 回退。具备 AI Cove 凭证且服务端私有协议已部署时再显式运行线上门禁：

```bash
cargo test --manifest-path src-tauri/Cargo.toml \
  live_ai_cove_websocket_handshake_passes_through_turbo -- --ignored

cargo test --manifest-path src-tauri/Cargo.toml \
  live_codex_request_uses_turbo_websocket -- --ignored
```

两个线上测试都使用隔离配置或环境变量，不应修改用户真实的 `~/.codex/config.toml`。此前标准 WebSocket 门禁已通过，脱敏记录见 `docs/verification/2026-08-03-websocket-live.md`；该记录早于私有 zstd 实现，不能作为公网腿 zstd 已生效的证据。

私有模式下，Codex→Turbo 回环握手不接受 `permessage-deflate`，Turbo→AI Cove 只提出 `ai-cove-zstd.v1` 且不提出 Extensions；只有服务端未接受该 subprotocol 时，Turbo 才重连标准透明 WS 并恢复 Codex 原始 `permessage-deflate` 头。发布前必须以 `websocketZstdVerified=true`、公网发送字节指标和服务端聚合字节共同证明真实 zstd 已发生；不新增第三个用户开关。
