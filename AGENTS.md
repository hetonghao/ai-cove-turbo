# Turbo Agent 工作规范

## Rust/Tauri 调试产物

本节适用于会生成 `src-tauri/target` 的 `cargo check`、`cargo test`、`cargo clippy`、benchmark、`tauri dev` 及相关验证任务。

### 调试前

1. 判断本次 Debug 产物是否需要跨后续命令复用。
2. 一次性验证优先使用任务专属目录，例如设置 `CARGO_TARGET_DIR=/tmp/ai-cove-turbo-target-<task-id>`，让临时产物与共享工作区隔离。
3. 若使用共享 `src-tauri/target`，明确记录本次需要保留的 `debug` 或 `release` 产物。

### 调试后完成条件

调试只有在以下条件同时满足时才算完成：

- 验证命令已经结束；
- 没有正在运行的 `cargo`、`rustc` 或 `tauri` 进程，也没有打开目标目录中文件的进程；
- 后续步骤不再需要本次生成的 Debug 产物。

满足条件后立即清理：

- 任务专属 `/tmp/...` target：删除整个任务目录；
- 共享 target：删除 `src-tauri/target/debug`，保留仍需交付或复用的 `src-tauri/target/release`。

每次调试交付前，在进度或结果中明确写出“已清理”的路径；确需保留时写出路径、当前大小、保留原因和下一次清理节点。
