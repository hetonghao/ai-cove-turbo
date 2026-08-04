# AI Cove Turbo 桌面应用设计约束

## 1. Atmosphere & Identity

桌面应用采用已确认的最新原型作为唯一视觉基线：实时页和统计页复用 C 的高密度运维控制台，配置页复用横向 B 面板。三个页面共用 Dot Field 背景，但各自保持明确的功能边界。

目标用户是需要低存在感配置入口的普通 Codex 用户，以及需要快速扫读路由、压缩与连接状态的技术用户。两类用户都必须能通过键盘完成 Tab 切换并清楚区分“配置动作”和“只读运行观测”。

## 2. Color

- B 基础色保持：`--b-bg #0b1010`、`--b-surface #151c1b`、`--b-surface-2 #1c2523`、`--b-ink #eff5ef`。
- C 基础色保持：`--c-bg #080c0b`、`--c-panel #101613`、`--c-panel-2 #141c18`、`--c-ink #e6f0e8`。
- 旧 B/C 的绿色强调统一替换为 AI Cove 海蓝：`--b-accent` 与 `--c-accent` 均为 `#70d8ee`；对应透明高光使用 `rgba(112, 216, 238, ...)`。
- 三页外壳使用独立 `--turbo-*` 语义令牌管理颜色、间距、字号、圆角、焦点和图标尺寸，不把外壳数值散落到组件规则中。
- 不修改原版其余中性色、暖色告警和深色层级，不引入新的渐变体系。

## 3. Typography

- 完整保留旧 B/C 的系统无衬线与系统等宽字体栈。
- 完整保留旧版字号、字重、字距、英文大写运维标签和数据数字样式。
- 中文状态文案避免单字孤行；路由与长字符串允许安全换行或省略。

## 4. Spacing & Layout

- 外层为 58px 品牌/Tab 栏加一个可滚动内容区，页面顺序固定为实时、统计、配置。
- 配置页使用最大宽度 980px 的横向 B 面板，左侧展示生效链路，右侧集中配置动作。
- 实时页只承载运行状态和最近 100 条真实请求；统计页只承载聚合指标、筛选、时间轴和滚动窗口。
- 实时与统计页不包含配置开关或重启动作；所有配置动作只在配置页出现。
- 375 / 768 / 1280px 均不得产生整页横向滚动。

## 5. Components

- `TurboShellHeader`：Turbo 图标、产品名、实时/统计/配置 Tab、预览标识。
- `Tabs`：原生按钮与 `tablist/tab/tabpanel` 语义，支持点击、左右方向键、Home、End。
- `BPopover`：状态、压缩与 WebSocket 独立开关、会话摘要、自启动、更新与重启 Codex；公网腿 zstd 复用 WebSocket 自动协商，不新增第三个开关。
- `LiveConsole`：只读路由、实时健康状态和最近 100 条真实请求。
- `StatisticsConsole`：按时间范围、传输方式和结果筛选进程内聚合；固定使用 6×10 秒、10×1 分钟、12×5 分钟、24×1 小时滚动桶。
- `TurboIcon`：使用 `assets/turbo-icon.png`，只作为品牌标识，不替代真实交互控件。

## 6. Motion & Interaction

- 保留旧 B/C 的 hover、focus、pressed 反馈。
- Tab 切换只改变可见面板与 URL 的 `?tab=live|statistics|config`，并兼容旧 `runtime|stats` 参数，不增加装饰动画。
- 配置动作通过 Tauri 命令更新真实运行状态；WebSocket 开关同时更新受管 Provider 的 `supports_websockets`，并提示重启 Codex Desktop 后验证。
- `prefers-reduced-motion: reduce` 下将交互过渡缩短为近即时。

## 7. Depth & Surface

- B 保留横向面板、圆角和阴影；三页共同透出 Dot Field 背景。
- C 保留全宽运维网格、线框分区和等宽数据层级。
- 外层 Tab 栏只使用低对比边线与轻微模糊，不覆盖或改写内层材质。

## 8. Accessibility Constraints & Accepted Debt

- 状态不能只靠颜色表达；所有开关保留文字状态与 `aria-pressed`。
- Tab 焦点顺序、选中状态和面板关联必须可被键盘与辅助技术识别。
- Turbo 图标在已有文字品牌旁作为装饰图，使用空替代文本。
- 已接受债务：实时请求和统计只保留当前进程内存，不做历史数据库；“速度提升约 31.6%”为明确占位，等待后续基准测试校准。
- 已接受债务：AI Cove private WebSocket 按上行消息实时记录；标准 WebSocket 隧道不解帧，仅在通道关闭时汇总真实上行字节，避免改变透传协议语义。
- 已接受债务：B/C 继承原型的局部颜色、间距和字号字面量，本票为保持用户确认的原样式不做全量令牌化；新增或调整的外壳与海蓝强调色必须使用语义令牌。
