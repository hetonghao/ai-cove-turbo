# AI Cove Turbo Context

AI Cove Turbo 是一个常驻桌面控制应用，用于管理 Codex 的本地请求加速能力和连接模式。

## Language

**Turbo**：AI Cove Turbo 桌面应用本身，负责运行状态、用户开关和 Codex 配置管理。
_Avoid_: 网关、脚本、代理（这些只表示 Turbo 的组成部分）。

**压缩模式**：Codex 请求先经过本机的 Turbo 请求通道，再以 zstd 压缩形式转发到 AI Cove 上游；它可以独立于 WebSocket 模式启用或关闭。
_Avoid_: Codex 原生压缩、传输压缩（当前模式由 Turbo 管理）。

**WebSocket 模式**：允许 Codex 对 `/v1/responses` 使用 WebSocket 通信的连接模式；它可以独立于压缩模式启用或关闭。
_Avoid_: SSE 模式、实时模式（它们不是同一个配置概念）。

**Hybrid 连接租约**：本地 WebSocket session 在 Hybrid 模式下对一个受管上游连接的唯一临时持有；租约可以完成请求、被安全丢弃，或在满足 continuation 条件时进入续传，不允许同一 session 同时持有两个上游连接。
_Avoid_: 连接缓存、可复制连接、已提交请求重放。

**压缩调度预算**：Turbo 在 HTTP 与私有 WebSocket 之间共享的有界压缩执行能力；等待预算不会改变协议语义，取消只放弃当前结果，不触发重放或静默降级。
_Avoid_: 全局限流、模型配额、网络带宽配额。

**性能证据 artifact**：绑定 Turbo 版本、工具链、运行 profile、固定 workload 指纹和策略常量的版本化性能比较结果；它用于发布前比较实现变化，不属于桌面状态或用户可见收益估算。
_Avoid_: 运行日志、用户请求记录、精确公网性能承诺。

**受管 Provider**：Codex 根配置中由 `model_provider` 指向、且允许 Turbo 修改连接相关配置的 Provider。
_Avoid_: 全量配置、所有 Provider（Turbo 不接管无关 Provider）。

**配置生效**：Codex 进程已经加载 Turbo 当前配置并实际使用对应连接；仅修改 `config.toml` 不等于配置生效。
_Avoid_: 文件已写入、配置已保存（它们只能表示写入状态）。

**恢复回退**：Turbo 正常退出时恢复接管前的 Codex 配置；异常退出留下的临时状态在 Turbo 下一次启动时修复。
_Avoid_: 常驻系统服务、强保证恢复（当前不做过度设计）。

**桌面应用重启**：只优雅退出并重新打开用户使用的 Codex 桌面应用，不操作 CLI、MCP 或其他独立 app-server 进程。
_Avoid_: 按进程名批量结束、重启所有 Codex 进程。

**配置验证**：配置写入、桌面应用重启和实际请求观测是三个不同状态；只有匹配的实际请求被 Turbo 观测到后，相关功能才标记为已验证。
_Avoid_: 只凭文件内容宣称网络功能已生效。

**首次启动**：Turbo 第一次运行时默认开启压缩模式和 WebSocket 模式，并接管受管 Provider；用户可以随后独立关闭任一功能。
_Avoid_: 首次启动默认关闭、要求用户先完成端点引导。

**开机自启动**：Turbo 首次运行时默认随系统登录启动，以保证受管 Provider 指向本地端点时对应服务可用；用户可以手动关闭。
_Avoid_: 首次启动默认关闭自启动。

**AI Cove 上游**：Turbo 的默认且主要兼容目标，标准端点为 `https://api.ai-cove.com/v1`。
_Avoid_: 把任意 OpenAI 兼容端点都表述为已受 Turbo 保证支持。

**非 AI Cove 上游**：主机不是 AI Cove 标准端点的用户自定义上游；Turbo 在明确警告后允许继续尝试连接，但相关配置可能不生效或产生错误。
_Avoid_: 静默使用、显示为完全兼容、无提示直接阻止。

**WebSocket 集成门禁**：Turbo 在本项目内完成 WebSocket 透明转发和本地验证；AI Cove 上游支持由其他工作流交付，生产端到端验证在两边完成后执行。
_Avoid_: 在 Turbo 项目中顺带修改 AI Cove 上游、未集成测试就宣称 WebSocket 可用。

**本地端点**：Turbo 仅在本机回环地址提供服务，优先复用上次成功端口；端口冲突时自动选择空闲端口，并在监听成功后更新受管 Provider。
_Avoid_: 因固定端口冲突中断启动、结束未知占用进程、绑定局域网地址。

**Codex 配置位置**：Turbo 首版只读取当前用户目录下的默认 `.codex/config.toml`；文件不存在、无法解析或缺少受管 Provider 时停止接管，并提示用户自行处理后重试。
_Avoid_: 自定义路径选择、读取 `CODEX_HOME`、扫描磁盘寻找配置、自动创建或修复 Provider。

**配置冲突**：Turbo 运行期间受管字段被外部修改后，外部值取得所有权；Turbo 不再覆盖或在退出时恢复这些字段，并向用户提示冲突。
_Avoid_: 用启动时的整份配置快照覆盖当前文件、覆盖外部修改。

**低存在模式**：Turbo 默认主要驻留在 macOS 菜单栏或 Windows 系统托盘；关闭设置窗口不停止服务。macOS 默认显示 Dock 图标，用户可以在设置中隐藏；Windows 打开设置窗口时正常显示任务栏入口。
_Avoid_: 关闭窗口即退出、必须长期占用 Dock 或任务栏位置。

**收益估算**：Turbo 基于实际流量、请求轮数和经基准测试校准的网络模型，估算节省流量与时间；估算值必须与真实观测值区分显示。
_Avoid_: 把 token 数直接换算成 WebSocket 节省时间、把估算值表述为精确结果。

**版本更新**：Turbo 从 AI Cove 托管的签名更新清单检查、下载并安装新版本，更新完成后重新启动应用。
_Avoid_: 未签名更新、以 GitHub Actions 临时产物作为正式客户端更新源。
