# AI Cove Turbo Download Entry Throwaway Design System

## 1. Atmosphere & Identity

这是一个只用于比较信息架构的首页下载入口原型，不是正式产品页。它沿用父级 Turbo 原型的深色开发者界面、AI Cove 的 ember action 与 channel blue identity；三种方案分别用轨道、双系统清单和发布终端表达同一个下载动作。

## 2. Color

颜色复用根目录 `DESIGN.md` 与父级 `turbo/prototype/DESIGN.md` 的 AI Cove 令牌，不在产品代码中散落新色值。

| Role | Token | Value | Usage |
| --- | --- | --- | --- |
| Canvas | `--canvas` | `#0b1220` | 页面背景 |
| Panel | `--panel` | `#111827` | 入口卡片、终端 |
| Panel soft | `--panel-soft` | `#172033` | 平台行、按钮 |
| Text | `--ink` / `--warm` | `#e7ebef` / `#fbfaf8` | 主文案、标题 |
| Muted | `--muted` | `#94a3b8` | 说明与元数据 |
| Border | `--line` | `#253246` | 结构分隔 |
| Channel | `--blue` / `--blue-deep` | `#0b8fc4` / `#075f96` | 路由、连接、产品身份 |
| Action | `--ember` / `--ember-lift` | `#c45100` / `#d47030` | 下载动作与选中态 |
| Success | `--success` | `#16a34a` | 通道准备状态 |

## 3. Typography

- Sans: `-apple-system, BlinkMacSystemFont, "SF Pro Display", "Segoe UI", sans-serif`。
- Mono: `"SFMono-Regular", Consolas, "Liberation Mono", monospace`，用于版本、路径、终端和平台元数据。
- 标题使用 `clamp()`，移动端不超过 4 行；正文不低于 14px。

## 4. Spacing & Layout

- 基础单位为 4px；页面使用 `--space-1` 到 `--space-16`。
- 内容最大宽度 1280px；断点为 720px 与 480px，覆盖 375 / 768 / 1280px。
- 主要内容为一列或双列的 intrinsic grid；固定底部切换器由主内容的底部留白避让。

## 5. Components

### Download Entry

- **Structure**: platform link / label / target metadata / prototype feedback。
- **Variants**: primary CTA（A）、platform rows（B）、console buttons（C）。
- **States**: default, hover, active, focus, prototype feedback。
- **Accessibility**: 使用真实 `<a href>` 与 `download` 语义；点击后用 `role=status` 说明当前为原型模拟。

### Variant Switcher

- **Structure**: `tablist` + three native `button[role=tab]` + hidden tabpanels。
- **States**: selected, hover, active, focus。
- **Accessibility**: `aria-selected`、`aria-controls`、roving `tabindex`；支持点击、左右方向键、Home、End。

## 6. Motion & Interaction

- 仅使用 `transform`、`opacity` 与颜色过渡；切换面板是短暂的 opacity/translate 入场。
- 下载入口的反馈只说明目标，不伪造真实安装状态。
- `prefers-reduced-motion: reduce` 下关闭非必要过渡与光标闪烁。

## 7. Depth & Surface

采用 mixed 策略：结构优先使用 `--line` 边线，顶层卡片使用父级 `Panel Night` 阴影；不使用玻璃材质作为默认表面，不使用渐变文字。

## 8. Accessibility Constraints & Accepted Debt

- 目标为 WCAG 2.2 AA：所有入口可见焦点、键盘可达、状态不只靠颜色表达，CJK 文案允许自然换行。
- 这是一次性 throwaway 原型：下载文件、签名、`latest.json`、平台自动更新和真实安装反馈均未接入；正式发布前应替换 `app.js` 的拦截反馈为真实产物 URL 与失败状态。
