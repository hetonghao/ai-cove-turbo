# Turbo 使用 Tauri 2 与 Rust 原生核心

AI Cove Turbo 采用 Tauri 2 作为桌面壳，使用 Rust 承担本地请求通道、zstd 压缩和生命周期管理，界面使用系统 WebView。选择这条路线是为了避免 Electron/Node 常驻运行时，降低后台资源占用并保留更高的本地性能上限；代价是跨平台打包和签名流程更复杂，Windows 需要 WebView2。
