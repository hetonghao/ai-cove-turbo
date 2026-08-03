# AI Cove Turbo 桌面发布

## 构建平台

`.github/workflows/desktop-release.yml` 使用原生 runner：

- `macos-14`：macOS arm64
- `windows-latest`：Windows x64

每个平台先执行 `npm run check` 和 `npm test`，再执行 Tauri 构建。平台产物上传后，在 Ubuntu 汇总 job 中合并并校验 `latest.json`。

## 签名 Secrets

工作流只读取以下 GitHub Actions Secrets，不生成、不写入仓库：

- `AI_COVE_TURBO_TAURI_SIGNING_PRIVATE_KEY`
- `AI_COVE_TURBO_TAURI_SIGNING_PRIVATE_KEY_PASSWORD`

签名私钥仍需由主 Agent/仓库管理员在 GitHub 仓库 Secrets 中配置。

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

## 主 Agent 必须接入的配置点

本子 Agent 未修改 `src-tauri/tauri.conf.json`、`src-tauri/Cargo.toml` 或 Cargo 源码。要让 updater 真正工作，主 Agent 需要：

1. 在 `src-tauri/tauri.conf.json` 的 `plugins` 增加 updater 配置：
   - `endpoints`: `https://ai-cove.com/downloads/turbo/latest.json`
   - `pubkey`: AI Cove Turbo 专用 updater 公钥
2. 在 `bundle` 中保留 `icons/32x32.png`、`icons/128x128.png`、`icons/128x128@2x.png`、`icons/icon.icns`、`icons/icon.ico`。
3. 在 `src-tauri/Cargo.toml` 增加并启用 `tauri-plugin-updater`；在 `src-tauri/src/main.rs` 注册该插件，并接入前端更新检查调用。
4. 用同一版本号同步 `src-tauri/Cargo.toml` 与 `src-tauri/tauri.conf.json`，使两个 runner 生成的 manifest 版本一致。

以上配置点刻意没有在本子 Agent 的提交中修改，避免覆盖并行 Agent 的 Rust/Tauri 工作。
