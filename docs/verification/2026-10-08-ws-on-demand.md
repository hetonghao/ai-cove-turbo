# WS 按需连接验证记录

日期：2026-10-08。范围：[已确认方案 1、2、4](../specs/2026-10-08-ws-on-demand.md)。对比基线：`905aaeff17acfb5504b8dc6351393d3789e720c7`。

## 实现与边界

- 删除启动预热及固定备用池，连接需求仅来自实际等待 WS 的请求。HTTP 会话不会自动领用连接。
- 能力关闭后停止补连，回收空闲和已暂存的续传连接；已提交请求完成后退出，不增加重放。
- 所有 Hybrid HTTP 入口在共同 worker 中处理 `generate=false`：本地终态、零上游生成、保存续传底稿，后续 HTTP 展开输入且移除传输字段。
- 展示和路由使用有效能力，过期按 HTTP；刷新失败不延长旧能力，新快照整体替换。
- 不改写 Codex provider 的 `supports_websockets`，不改变用户全局 WS 偏好。

## 验证结果

| 检查 | 结果 |
| --- | --- |
| `node --test tests/*.test.mjs` | 147 通过 |
| `cargo test --manifest-path src-tauri/Cargo.toml` | 458 通过，1 失败，7 忽略 |
| 本次 WS / HTTP 预热、续传、取消、隔离、能力切换测试 | 全部通过 |
| `cargo clippy --manifest-path src-tauri/Cargo.toml --all-targets` | 未通过；已有 75 个错误，最终诊断按级别、消息、文件与基线比较无新增 |
| 修改文件 rustfmt、`git diff --check` | 通过 |
| 静态前端浏览器检查 | DOM 中 HTTP-only、过期 HTTP、WS 可用标签与预期一致 |
| 双轴只读审查 | Spec、Standards 最终均通过 |

Rust 唯一失败为 `skills_tests::real_imagine_release_installs_via_http_fixture`：固定断言预期版本 `1.0.0`，当前平台 Imagine 技能版本是 `1.0.6`。在改动前基线独立归档中已复现同一失败；本次没有修改 Skills 代码或该断言。Clippy 的现存错误主要涉及 config、catalog、skills 等既有代码；不把本次检查表述为全绿。

审查发现并修复了三处边界，并各自确认回归测试能捕获问题：已就绪的停止信号先于连接创建；连续发送本地预热与取消不会关闭客户端 WS；能力关闭后在移交窗口到期前回收暂存连接。最后一项通过临时禁用能力回收分支确认失败，再恢复修复确认通过。

## 发布状态与环境

只提交本地 Turbo 当前分支；未发布、未部署、未重启运行中的 Turbo 或 Codex。浏览器验证使用静态页面和状态 fixture；未声称完成真实 Codex 客户端端到端验证。

Rust 构建和测试使用任务专属 `CARGO_TARGET_DIR=/tmp/ai-cove-turbo-target-ws-demand`；任务结束前清理该目录，保留其他共享构建目录。图谱更新输出按仓库既有忽略规则保留，不纳入提交。
