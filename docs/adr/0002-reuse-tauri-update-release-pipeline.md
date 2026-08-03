# 复用现有 Tauri 更新与双端发布流程

AI Cove Turbo 复用 AI Cove Design 已验证的 macOS/Windows 原生 Runner、平台产物收集、合并 `latest.json` 和发布前校验流程，并参考 Two Sides 的检查、下载、安装和重启交互。Turbo 使用独立的 Tauri 更新签名密钥和产品命名，不复制 AI Cove Design 的 Node sidecar 打包逻辑，也不重新设计一套更新协议。
