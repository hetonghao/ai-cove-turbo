# AI Cove Turbo 桌面应用设计约束

## 1. Atmosphere & Identity

桌面应用采用上一版两个已确认界面作为唯一视觉基线：配置页复用 B 的紧凑菜单栏弹层，运行页复用 C 的高密度运维控制台。双 Tab 只负责切页，不统一或重绘两个页面的背景、布局、圆角、字体与信息密度。

目标用户是需要低存在感配置入口的普通 Codex 用户，以及需要快速扫读路由、压缩与连接状态的技术用户。两类用户都必须能通过键盘完成 Tab 切换并清楚区分“配置动作”和“只读运行观测”。

## 2. Color

- B 基础色保持：`--b-bg #0b1010`、`--b-surface #151c1b`、`--b-surface-2 #1c2523`、`--b-ink #eff5ef`。
- C 基础色保持：`--c-bg #080c0b`、`--c-panel #101613`、`--c-panel-2 #141c18`、`--c-ink #e6f0e8`。
- 旧 B/C 的绿色强调统一替换为 AI Cove 海蓝：`--b-accent` 与 `--c-accent` 均为 `#70d8ee`；对应透明高光使用 `rgba(112, 216, 238, ...)`。
- 双 Tab 外壳使用独立 `--turbo-*` 语义令牌管理颜色、间距、字号、圆角、焦点和图标尺寸，不把外壳数值散落到组件规则中。
- 不修改原版其余中性色、暖色告警和深色层级，不引入新的渐变体系。

## 3. Typography

- 完整保留旧 B/C 的系统无衬线与系统等宽字体栈。
- 完整保留旧版字号、字重、字距、英文大写运维标签和数据数字样式。
- 中文状态文案避免单字孤行；路由与长字符串允许安全换行或省略。

## 4. Spacing & Layout

- 外层为 58px 品牌/Tab 栏加一个可滚动内容区。
- 配置 Tab 中 B 弹层继续居中，最大宽度 390px。
- 运行 Tab 中 C 保持顶栏、标题、左侧路由、指标、趋势和事件表结构。
- 运行页不包含配置开关或重启动作；所有配置动作只在 B 出现。
- 375 / 768 / 1280px 均不得产生整页横向滚动。

## 5. Components

- `TurboShellHeader`：Turbo 图标、产品名、配置/运行 Tab、预览标识。
- `Tabs`：原生按钮与 `tablist/tab/tabpanel` 语义，支持点击、左右方向键、Home、End。
- `BPopover`：状态、压缩与 WebSocket 独立开关、会话摘要、自启动、更新与重启 Codex；公网腿 zstd 复用 WebSocket 自动协商，不新增第三个开关。
- `CConsole`：只读路由、运行指标、传输图和事件表。
- `TurboIcon`：使用 `assets/turbo-icon.png`，只作为品牌标识，不替代真实交互控件。

## 6. Motion & Interaction

- 保留旧 B/C 的 hover、focus、pressed 反馈。
- Tab 切换只改变可见面板与 URL 的 `?tab=config|runtime`，不增加装饰动画。
- 配置动作通过 Tauri 命令更新真实运行状态；WebSocket 开关同时更新受管 Provider 的 `supports_websockets`，并提示重启 Codex Desktop 后验证。
- `prefers-reduced-motion: reduce` 下将交互过渡缩短为近即时。

## 7. Depth & Surface

- B 保留菜单栏弹层、网格背景、圆角和阴影。
- C 保留全宽运维网格、线框分区和等宽数据层级。
- 外层 Tab 栏只使用低对比边线与轻微模糊，不覆盖或改写内层材质。

## 8. Accessibility Constraints & Accepted Debt

- 状态不能只靠颜色表达；所有开关保留文字状态与 `aria-pressed`。
- Tab 焦点顺序、选中状态和面板关联必须可被键盘与辅助技术识别。
- Turbo 图标在已有文字品牌旁作为装饰图，使用空替代文本。
- 已接受债务：运行指标只保留当前进程内的聚合值，不做历史数据库；节省时间估算要等后续基准校准后再展示。
- 已接受债务：B/C 继承原型的局部颜色、间距和字号字面量，本票为保持用户确认的原样式不做全量令牌化；新增或调整的外壳与海蓝强调色必须使用语义令牌。
