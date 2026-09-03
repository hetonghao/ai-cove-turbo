import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const sourceUrl = new URL("../src/", import.meta.url);

test("桌面壳按实时、统计、配置三页承载观测与控制", async () => {
  // Given: Turbo 的生产前端入口。
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const css = await readFile(new URL("styles.css", sourceUrl), "utf8");
  const app = await readFile(new URL("app.js", sourceUrl), "utf8");
  const packageJson = JSON.parse(await readFile(new URL("../package.json", sourceUrl), "utf8"));

  // When: 用户打开设置窗口。
  const tabs = html.match(/role="tab"/g) ?? [];
  const livePanel = html.slice(html.indexOf('id="panel-live"'), html.indexOf('id="panel-statistics"'));
  const statisticsPanel = html.slice(html.indexOf('id="panel-statistics"'), html.indexOf('id="panel-config"'));
  const configPanel = html.slice(html.indexOf('id="panel-config"'));
  const versionBarIndex = configPanel.indexOf('class="b-version-bar"');
  const configStageIndex = configPanel.indexOf('class="b-stage"');
  const configCardIndex = configPanel.indexOf('class="b-popover b-popover--wide');
  const versionBarEndIndex = configPanel.indexOf("</header>", versionBarIndex);
  const updateProgressIndex = configPanel.indexOf('class="b-progress', versionBarIndex);

  // Then: 实时、统计和配置页可访问，业务控制仍只出现在配置页。
  assert.equal((html.match(/data-tab="/g) ?? []).length, 3);
  assert.equal(tabs.length, 5);
  assert.match(html, /data-tab="live"/);
  assert.match(html, /data-tab="statistics"/);
  assert.match(html, /data-tab="config"/);
  assert.match(html, /data-request-stream/);
  assert.match(html, /data-stat-bars/);
  assert.match(statisticsPanel, /data-stat="speed-gain"/);
  assert.match(statisticsPanel, /基准模型估算/);
  assert.match(statisticsPanel, /非当前请求实测/);
  assert.match(statisticsPanel, /data-filter="range"/);
  assert.match(statisticsPanel, /data-filter="transport"/);
  assert.match(statisticsPanel, /data-filter="result"/);
  assert.match(html, /assets\/turbo-icon\.png/);
  assert.match(html, /data-state="endpoint"/);
  assert.match(html, /data-state="config-message"/);
  assert.match(html, /data-state="provider"/);
  assert.match(html, /data-state="upstream"/);
  assert.match(html, /data-state="http-zstd-runtime"/);
  assert.match(html, /data-action="toggle-websocket"/);
  assert.match(html, /data-state="websocket"/);
  assert.match(html, /data-state="websocket-runtime"/);
  assert.match(html, /data-state="websocket-zstd-runtime"/);
  assert.match(html, /data-state="websocket-handshakes"/);
  assert.match(html, /data-state="http-fallbacks"/);
  assert.match(livePanel, /data-state="hybrid-ws"/);
  assert.match(livePanel, /data-state="hybrid-cold-start-http"/);
  assert.match(livePanel, /data-state="hybrid-recovery-http"/);
  assert.match(livePanel, /data-state="direct-http"/);
  assert.match(livePanel, /data-action="reset-route-metrics"/);
  assert.match(livePanel, /aria-controls="route-reset-popover"[^>]*aria-expanded="false"/);
  assert.match(livePanel, /data-action="cancel-route-reset"/);
  assert.match(livePanel, /data-action="confirm-route-reset"/);
  assert.match(livePanel, /class="turbo-update-bubble__tail c-route-reset-popover__tail" viewBox="0 0 24 16"/);
  assert.match(livePanel, /class="turbo-bubble-action turbo-bubble-action--secondary"[^>]*data-action="cancel-route-reset"/);
  assert.match(livePanel, /class="turbo-bubble-action turbo-bubble-action--primary"[^>]*data-action="confirm-route-reset"/);
  assert.match(html, /data-action="toggle-autostart"/);
  assert.match(html, /data-action="toggle-dock"/);
  assert.match(html, /data-action="restart-codex"/);
  assert.match(html, /data-action="retry-takeover"/);
  assert.match(html, /data-action="set-ai-cove-upstream"/);
  assert.match(html, /data-visible="ai-cove-upstream"/);
  assert.match(html, /data-action="confirm-non-ai-cove"/);
  assert.match(html, /data-action="check-for-updates"/);
  assert.match(html, /data-action="install-update"/);
  assert.match(html, /class="b-version-bar__current-mark" data-update-current-mark/);
  assert.match(html, /<path pathLength="1" d="m2\.5 8\.4 3\.2 3\.1 7\.3-7" \/>/);
  assert.match(html, /data-update-bubble-slot hidden/);
  assert.match(html, /class="turbo-update-bubble turbo-tilt-bubble"/);
  assert.match(livePanel, /class="c-route-reset-popover turbo-tilt-bubble"/);
  assert.match(html, /data-action="install-update-bubble"/);
  assert.match(html, /data-action="ignore-update-bubble"/);
  assert.match(html, /更新已准备好🎉/);
  assert.match(css, /\.turbo-update-bubble-slot\s*\{[\s\S]*?top:\s*calc\(100% \+ var\(--turbo-update-bubble-offset-block\)\);[\s\S]*?left:\s*var\(--turbo-update-bubble-offset-inline\);/);
  assert.match(app, /checkUpdatesOncePerDay/);
  assert.match(app, /function renderUpdateCurrentMark/);
  assert.match(app, /renderedUpdateState !== "current"/);
  assert.match(css, /\.b-version-bar__current-mark\.is-entering\s*\{[\s\S]*?animation: turbo-current-mark-pop 360ms/);
  assert.match(css, /@keyframes turbo-current-mark-draw/);
  assert.match(app, /function bindTiltBubble/);
  assert.match(app, /function openConnectionHoverCard/);
  assert.match(app, /function positionConnectionHoverCard/);
  assert.match(app, /c-hover-card--portal/);
  assert.match(app, /const hasGlareEffect = Boolean\(glare \|\| \(tail && gradient\) \|\| bubble\.dataset\.tiltGlare === "true"\)/);
  assert.match(app, /function bindTiltBubble\([\s\S]*maxTiltDegrees = 14/);
  assert.match(app, /bubble\.dataset\.tiltGlare === "true"/);
  assert.match(app, /--tilt-glare-x/);
  assert.match(app, /UPDATE_PREFERENCE_KEY/);
  assert.match(app, /setUpdateBubbleOpen\(false\)/);
  assert.match(app, /setUpdateBubbleOpen\(false\);[\s\S]*?data-ai-cove-trigger.*?focus/);
  assert.match(app, /install-update-bubble'[\s\S]*?setUpdateBubbleOpen\(false\);[\s\S]*?selectTab\("config", \{ focus: true \}\)/);
  assert.match(css, /\.turbo-update-bubble__tail\s*\{[\s\S]*?top:\s*-16px;[\s\S]*?left:\s*13px;[\s\S]*?width:\s*24px;[\s\S]*?height:\s*16px;/);
  assert.match(html, /class="turbo-update-bubble__tail" viewBox="0 0 24 16"/);
  assert.match(css, /\.turbo-update-bubble__tail-stroke\s*\{[\s\S]*?stroke:\s*var\(--turbo-update-bubble-border\);[\s\S]*?stroke-width:\s*1\.2px;/);
  assert.match(css, /\.turbo-update-bubble__tail-highlight\s*\{[\s\S]*?mix-blend-mode:\s*screen;/);
  assert.match(css, /\.turbo-update-bubble__glare,\s*\.c-route-reset-popover::before/);
  assert.match(css, /@media \(prefers-reduced-motion: reduce\)[\s\S]*?\.turbo-tilt-bubble\s*\{[\s\S]*?transform:\s*none !important;[\s\S]*?\}/);
  assert.doesNotMatch(css, /@media \(prefers-reduced-motion: reduce\)[\s\S]*?\.turbo-update-bubble,\s*\.turbo-update-bubble__glare/);
  assert.ok(versionBarIndex >= 0 && versionBarIndex < configStageIndex && configStageIndex < configCardIndex);
  assert.ok(configPanel.includes(`>v${packageJson.version}</span>`));
  assert.equal(configPanel.match(/data-action="check-for-updates"/g)?.length, 1);
  assert.equal(configPanel.match(/data-state="update-state"/g)?.length, 1);
  assert.ok(updateProgressIndex > versionBarIndex && updateProgressIndex < versionBarEndIndex);
  assert.match(configPanel, /class="b-version-bar__percent[^>]*data-state="update-progress"/);
  assert.match(configPanel, /class="b-popover b-popover--wide turbo-tilt-bubble" data-tilt-only/);
  assert.match(configPanel, /class="b-model-catalog b-model-catalog--wide turbo-tilt-bubble" data-tilt-only/);
  assert.match(app, /all\("\[data-tilt-only\]"\)\.forEach\(\(bubble\) => bindTiltBubble\(\{ bubble, hitArea: bubble, maxTiltDegrees: 7 \}\)\)/);
  assert.match(css, /\.b-config-page__header\s*\{[\s\S]*?margin:\s*0 0 clamp\(12px, 2\.4vh, 24px\);/);
  assert.match(css, /\.b-model-catalog--wide\s*\{[\s\S]*?background:\s*rgba\(15, 22, 18, 0\.6\);/);
  assert.match(css, /\.b-version-bar \.b-progress\s*\{[^}]*position: absolute;/s);
  assert.match(css, /transform: scaleX\(var\(--progress, 0\)\)/);
  assert.match(css, /\.c-route-metrics__actions\s*\{[\s\S]*?top:\s*-28px;/);
  assert.match(css, /\.c-route-reset-popover\s*\{[\s\S]*?position:\s*fixed;[\s\S]*?overflow:\s*visible;/);
  assert.match(css, /\.c-route-reset-popover strong\s*\{[\s\S]*?color:\s*var\(--turbo-accent\);/);
  assert.match(css, /\.turbo-bubble-action\s*\{[\s\S]*?min-height:\s*34px;[\s\S]*?transition: transform var\(--speed-fast\)/);
  assert.match(css, /\.c-route-reset-popover \.c-route-reset-popover__tail\s*\{[\s\S]*?bottom:\s*-12px;[\s\S]*?width:\s*18px;[\s\S]*?height:\s*12px;/);
  assert.match(css, /\.c-route-reset-popover \.turbo-update-bubble__tail-stroke\s*\{[\s\S]*?stroke:\s*var\(--turbo-accent-line\);[\s\S]*?stroke-width:\s*1px;/);
  assert.match(css, /--turbo-danger:\s*#ff7b74;/);
  assert.match(css, /\.turbo-bubble-action--danger\s*\{[\s\S]*?background:\s*var\(--turbo-danger\);/);
  assert.match(css, /\.c-transport__tooltip\s*\{[\s\S]*?max-height:\s*min\(420px, calc\(100dvh - 32px\)\);[\s\S]*?overflow-y:\s*auto;/);
  assert.match(css, /--turbo-detail-viewport-inset:\s*32px;/);
  assert.match(css, /\.c-hover-card\s*\{[\s\S]*?width:\s*max-content;[\s\S]*?min-width:\s*min\(var\(--turbo-hover-card-min-width\), calc\(100vw - var\(--turbo-detail-viewport-inset\)\)\);[\s\S]*?max-width:\s*min\(var\(--turbo-hover-card-max-width\), calc\(100vw - var\(--turbo-detail-viewport-inset\)\)\);/);
  assert.match(css, /\.c-hover-card--portal\s*\{[\s\S]*?position:\s*fixed;[\s\S]*?top:\s*var\(--c-hover-card-top, 8px\);[\s\S]*?left:\s*var\(--c-hover-card-left, 8px\);/);
  assert.match(css, /body\[data-connection-hover-card="true"\] \.c-connection-inspector \.c-hover-card\s*\{[\s\S]*?visibility:\s*hidden !important;/);
  assert.match(css, /\.c-connection-group__hint::after\s*\{[\s\S]*?max-width:\s*min\(260px, calc\(100vw - var\(--turbo-detail-viewport-inset\)\)\);/);
  assert.match(css, /\.c-transport__tooltip\s*\{[\s\S]*?width:\s*max-content;[\s\S]*?min-width:\s*min\(var\(--turbo-request-tooltip-min-width\), calc\(100vw - var\(--turbo-detail-viewport-inset\)\)\);[\s\S]*?max-width:\s*min\(var\(--turbo-request-tooltip-max-width\), calc\(100vw - var\(--turbo-detail-viewport-inset\)\)\);/);
  assert.match(css, /\.c-transport__tooltip\s*\{[\s\S]*?z-index:\s*var\(--turbo-focus-layer\);[\s\S]*?background:\s*var\(--turbo-tooltip-bg\);[\s\S]*?visibility:\s*hidden;/);
  assert.match(css, /\.c-transport__tooltip\.is-visible\s*\{[\s\S]*?opacity:\s*1;[\s\S]*?visibility:\s*visible;/);
  assert.doesNotMatch(css, /\.b-progress\s*\{[^}]*height: 18px;/s);
  assert.doesNotMatch(configPanel.slice(configCardIndex), /data-action="check-for-updates"|data-state="update-state"/);
  assert.doesNotMatch(html, /00:09:42|STREAMING/);
  assert.match(`${html}\n${css}\n${app}`, /扩展由上游协商/);
  assert.doesNotMatch(`${html}\n${css}\n${app}`, /permessage-deflate/i);
  assert.doesNotMatch(livePanel, /data-action="toggle-(compression|websocket|autostart|dock)"/);
  assert.doesNotMatch(statisticsPanel, /data-action=/);
  assert.match(livePanel, /data-live-recovery/);
  assert.match(livePanel, /data-restart-hint/);
  assert.match(app, /handleChartKeydown/);
  assert.doesNotMatch(livePanel, /c-topbar|AI COVE TURBO/);
  assert.doesNotMatch(statisticsPanel, /c-topbar|AI COVE TURBO/);
  assert.doesNotMatch(configPanel, /b-header|turbo-icon--popover/);
  assert.doesNotMatch(css, /\.a-|variant--a|turbo-variant-switcher|app-shell|data-variant/);
});

test("macOS 原生按钮覆盖在应用导航内且不再显示独立标题栏", async () => {
  // Given: Turbo 的桌面窗口配置与顶部导航。
  const tauriConfig = JSON.parse(
    await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  );
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const css = await readFile(new URL("styles.css", sourceUrl), "utf8");

  // When: macOS 创建主窗口。
  const mainWindow = tauriConfig.app.windows[0];

  // Then: 保留原生窗口按钮，把内容延伸到标题栏，并为按钮留出安全区。
  assert.equal(mainWindow.decorations, true);
  assert.equal(mainWindow.titleBarStyle, "Overlay");
  assert.equal(mainWindow.hiddenTitle, true);
  assert.deepEqual(mainWindow.trafficLightPosition, { x: 16, y: 19 });
  assert.match(html, /<header class="turbo-shell__header" data-tauri-drag-region="deep">/);
  assert.doesNotMatch(html, /<(?:a|button)[^>]*data-tauri-drag-region/);
  assert.match(html, /classList\.add\("is-macos-overlay"\)/);
  assert.match(css, /\.is-macos-overlay \.turbo-shell__brand\s*\{[^}]*margin-left: var\(--turbo-titlebar-controls-inset\)/);
});

test("macOS 导航拖拽显式开放主窗口权限", async () => {
  const capability = JSON.parse(
    await readFile(new URL("../src-tauri/capabilities/main.json", import.meta.url), "utf8"),
  );

  assert.deepEqual(capability.windows, ["main"]);
  assert.ok(capability.permissions.includes("core:default"));
  assert.ok(capability.permissions.includes("core:window:allow-start-dragging"));
});

test("产品图标显示临时 AI Cove 入口而不再刷新页面", async () => {
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const css = await readFile(new URL("styles.css", sourceUrl), "utf8");
  const app = await readFile(new URL("app.js", sourceUrl), "utf8");
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

  assert.doesNotMatch(html, /turbo-shell__brand" href=/);
  assert.match(html, /data-action="toggle-ai-cove-bubble"[^>]*data-ai-cove-trigger/);
  assert.match(html, /data-action="open-ai-cove"[^>]*data-ai-cove-bubble[^>]*hidden/);
  assert.match(css, /\.turbo-shell__brand-bubble\s*{[^}]*position:\s*absolute[^}]*top:\s*calc\(100% \+ 9px\)[^}]*left:\s*0/s);
  assert.match(css, /@keyframes turbo-brand-bubble-in\s*{[^}]*transform:\s*translateY\(-5px\)/s);
  assert.match(app, /invoke\("open_ai_cove"\)/);
  assert.match(rust, /OPEN_AI_COVE_MENU_ID => \{\s*let _open_result = open_ai_cove_url\(\);/s);
  assert.match(rust, /#\[tauri::command\]\s*fn open_ai_cove\(\) -> Result<\(\), String>/s);
});

test("实时页把同类路径计数收进一行并仅在需要时显示侧栏滚动条", async () => {
  // Given: 默认桌面窗口中的实时页侧栏。
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const css = await readFile(new URL("styles.css", sourceUrl), "utf8");
  const livePanel = html.slice(html.indexOf('id="panel-live"'), html.indexOf('id="panel-statistics"'));
  const metrics = livePanel.slice(livePanel.indexOf('class="c-route-metrics"'), livePanel.indexOf("</ul>", livePanel.indexOf('class="c-route-metrics"')));

  // When: 四类路由计数与健康状态同时展示。
  // Then: 路由计数横向合并，默认高度不预留滚动条轨道。
  assert.equal(metrics.match(/data-state=/g)?.length, 4);
  assert.match(metrics, />Hybrid <small>WS<\/small>/);
  assert.match(metrics, />首轮 <small>HTTP<\/small>/);
  assert.match(metrics, />回退 <small>HTTP<\/small>/);
  assert.match(metrics, />压缩 <small>HTTP<\/small>/);
  assert.match(css, /\.c-route-metrics\s*{[\s\S]*?grid-template-columns:\s*repeat\(4,/);
  assert.match(css, /\.c-sidebar--live\s*{[\s\S]*?scrollbar-gutter:\s*auto;/);
  assert.match(css, /@media \(max-width: 520px\)\s*{[^}]*\.c-console--live > \.c-titlebar\s*{[^}]*padding-top:\s*calc\(var\(--turbo-connection-summary-height\) \+ 20px\)/s);
});

test("实时页只保留一处配置生效状态", async () => {
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const app = await readFile(new URL("app.js", sourceUrl), "utf8");
  const livePanel = html.slice(html.indexOf('id="panel-live"'), html.indexOf('id="panel-statistics"'));

  assert.match(livePanel, /data-state="config-prerequisite"/);
  assert.doesNotMatch(livePanel, /data-state="config-runtime"/);
  assert.doesNotMatch(app, /"config-runtime"/);
});

test("Tauri 前端通过约定命令读取和修改真实状态", async () => {
  // Given: Rust 后端暴露的桌面命令契约。
  const app = await readFile(new URL("app.js", sourceUrl), "utf8");
  const commands = [
    "get_app_status",
    "open_ai_cove",
    "set_compression",
    "set_websocket",
    "set_autostart",
    "set_dock_visible",
    "restart_codex",
    "retry_takeover",
    "set_ai_cove_upstream",
    "confirm_non_ai_cove",
    "reset_route_metrics",
    "check_for_updates",
    "install_update",
    "update_model_catalog",
    "save_model_settings",
    "discover_model_catalog",
  ];

  // When: 前端加载并进入真实桌面运行时。
  // Then: 所有动作走 invoke，状态每秒刷新，浏览器预览仍有明确降级路径。
  for (const command of commands) assert.match(app, new RegExp(`['"]${command}['"]`));
  assert.match(app, /window\.__TAURI__\?\.core\?\.invoke/);
  assert.match(app, /setInterval\([^,]+,\s*1_000\)/s);
  assert.match(app, /Preview/);
  assert.match(app, /setRouteResetOpen/);
  assert.match(app, /document\.body\.appendChild\(bubble\)/);
  assert.match(app, /\[data-route-reset\], \[data-route-reset-popover\]/);
  assert.doesNotMatch(app, /window\.confirm/);
  assert.match(app, /enabled/);
  assert.match(app, /visible/);
  assert.match(app, /command === "confirm_non_ai_cove"\) state\.nonAiCoveConfirmed = true/);
  assert.match(app, /"set-ai-cove-upstream": \["set_ai_cove_upstream"\]/);
});

test("模型目录页面保留 Codex 可见性语义并提供稳定拖拽保存", async () => {
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const css = await readFile(new URL("styles.css", sourceUrl), "utf8");
  const app = await readFile(new URL("app.js", sourceUrl), "utf8");
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  const configPanel = html.slice(html.indexOf('id="panel-config"'));
  const globalSettings = configPanel.slice(configPanel.indexOf('data-config-view-panel="settings"'), configPanel.indexOf('data-config-view-panel="catalog"'));

  assert.match(configPanel, /role="tablist" aria-label="配置工作区"/);
  assert.match(configPanel, /<span>运行偏好<\/span>/);
  assert.doesNotMatch(configPanel, /CONFIG \/ WORKSPACES|运行配置和模型候选分别进入独立工作区/);
  assert.match(configPanel, /aria-selected="true"[^>]*data-config-view="settings"/);
  assert.match(configPanel, /aria-selected="false"[^>]*data-config-view="catalog"/);
  assert.match(configPanel, /data-config-view-panel="settings"/);
  assert.match(configPanel, /data-config-view-panel="catalog"[^>]*hidden/);
  assert.match(configPanel, /data-model-catalog-list/);
  assert.match(configPanel, /b-model-catalog b-model-catalog--wide/);
  assert.doesNotMatch(globalSettings, /data-model-catalog-list/);
  assert.match(configPanel, /data-model-catalog-info/);
  assert.match(configPanel, /选择显示给 Codex 的模型，并设置每个模型的传输方式。/);
  assert.match(configPanel, /data-action="save-model-settings"[^>]*disabled/);
  assert.match(configPanel, /data-action="undo-model-settings"[^>]*disabled/);
  assert.match(configPanel, /data-model-catalog-restart/);
  assert.match(configPanel, /data-model-catalog-actions[^>]*hidden/);
  assert.match(configPanel, /data-model-catalog-draft-actions[^>]*hidden/);
  assert.match(configPanel, /class="c-route-reset-popover c-model-delete-popover turbo-tilt-bubble" data-model-delete-popover/);
  assert.match(configPanel, /data-model-delete-popover data-tilt-glare="true"/);
  assert.doesNotMatch(configPanel, /data-model-delete-popover[\s\S]*?turbo-update-bubble__tail/);
  assert.match(configPanel, /class="turbo-bubble-action turbo-bubble-action--danger"[^>]*data-action="confirm-model-delete"/);
  assert.doesNotMatch(app, /已删除，需要重启 Codex 后生效/);
  assert.match(configPanel, /data-action="cancel-model-delete"/);
  assert.match(configPanel, /data-action="confirm-model-delete"/);
  assert.match(app, /Codex 新版本已删除此模型，建议用户删除/);
  assert.match(app, /此模型为 Codex 预设模型，暂不支持删除/);
  assert.match(app, /modelRootPresence/);
  assert.match(app, /disabled aria-disabled="true"/);
  assert.match(css, /\.b-model-row__root-status\s*\{[\s\S]*?color:\s*var\(--turbo-danger\);/);
  assert.match(css, /\.b-model-row__delete:disabled\s*\{[\s\S]*?cursor:\s*not-allowed;[\s\S]*?opacity:\s*0\.45;/);
  assert.doesNotMatch(configPanel, /同步根目录|手动同步/);
  assert.match(configPanel, /data-action="save-model-settings"[^>]*disabled hidden/);
  assert.match(configPanel, /data-action="undo-model-settings"[^>]*disabled hidden/);
  assert.match(configPanel, /data-action="restart-codex"[^>]*data-catalog-restart/);
  assert.match(app, /data-model-visibility-toggle/);
  assert.match(app, /data-model-drag-handle/);
  assert.match(app, /b-model-row__actions/);
  assert.match(app, /catalog\.path \|\| "~\/\.codex\/model-catalogs\/ai_cove_turbo\.json"/);
  assert.match(app, /class="b-model-row" draggable="false"/);
  assert.match(app, /class="b-transport-toggle" role="group"/);
  assert.match(app, /data-model-transport="auto"/);
  assert.match(app, /data-model-transport="http"/);
  assert.match(app, /aria-pressed="\$\{String\(transport === "auto"\)\}"/);
  assert.doesNotMatch(app, /data-model-visibility[^-]/);
  assert.match(app, /document\.addEventListener\("drop"/);
  assert.match(app, /document\.addEventListener\("pointerdown"/);
  assert.match(app, /document\.addEventListener\("pointerup"/);
  assert.match(app, /models\.splice\(to, 0, moved\)/);
  assert.match(app, /classList\?\.add\?\.\("is-dragging"\)/);
  assert.match(app, /is-drop-target/);
  assert.match(css, /\.b-model-catalog__actions button\s*\{[\s\S]*?border:\s*1px solid var\(--b-line\);[\s\S]*?background:\s*var\(--b-surface-2\);/);
  assert.match(css, /\.b-model-catalog__actions button:hover:not\(:disabled\)\s*\{[\s\S]*?color:\s*var\(--b-accent\);[\s\S]*?background:\s*var\(--b-accent-soft\);/);
  assert.match(css, /\.b-model-catalog__actions button:focus-visible\s*\{[\s\S]*?outline:\s*2px solid var\(--b-accent\);/);
  assert.match(css, /\.b-model-catalog__actions button:disabled\s*\{[\s\S]*?cursor:\s*not-allowed;[\s\S]*?opacity:\s*0\.45;/);
  assert.match(css, /\.b-model-catalog__draft-actions\s*\{[\s\S]*?background:\s*var\(--turbo-model-editor-footer-bg\);/);
  assert.match(css, /\.b-model-catalog__draft-actions\s*\{[^}]*?border:\s*0;[^}]*?background:\s*var\(--turbo-model-editor-footer-bg\);/);
  assert.match(css, /\.b-model-catalog__draft-actions \.turbo-bubble-action--primary\s*\{[\s\S]*?background:\s*var\(--turbo-accent\);/);
  assert.match(css, /\.b-model-catalog__draft-actions \.turbo-bubble-action--secondary\s*\{[\s\S]*?background:\s*transparent;/);
  assert.match(css, /\.b-model-catalog\[data-dirty="true"\] \.b-model-catalog__list\s*\{[\s\S]*?padding-bottom:\s*72px;[\s\S]*?scroll-padding-block-end:\s*72px;/);
  assert.match(css, /\.b-model-catalog__info\s*\{/);
  assert.match(css, /\.b-model-catalog__info::after\s*\{[\s\S]*?content:\s*attr\(data-tooltip\);/);
  assert.match(css, /\.b-model-catalog__info\s*\{[\s\S]*?color:\s*var\(--b-accent\);[\s\S]*?background:\s*var\(--b-accent-soft\);/);
  assert.match(css, /#panel-config \.b-hint\s*\{[\s\S]*?margin-top:\s*18px;/);
  assert.match(css, /body\[data-active-tab="config"\]\[data-config-view="catalog"\] #panel-config \.b-hint\s*\{[\s\S]*?margin-block-start:\s*18px;/);
  assert.doesNotMatch(app, /有模型的上下文上限仍待确认，请编辑后再保存。/);
  assert.match(css, /\.b-model-row__visibility\s*\{/);
  assert.match(css, /\.b-model-catalog__list\s*\{[\s\S]*?grid-template-columns:\s*repeat\(2,\s*minmax\(0,\s*1fr\)\)/);
  assert.match(css, /\.b-model-catalog__toolbar\s*\{[\s\S]*?align-items:\s*center;[\s\S]*?line-height:\s*1\.2;/);
  assert.match(css, /\.b-model-row\s*\{[\s\S]*?grid-template-areas:\s*"drag copy actions"/);
  assert.match(css, /\.b-model-row__drag\s*\{[\s\S]*?align-self:\s*center;/);
  assert.match(css, /\.b-model-row__copy\s*\{[\s\S]*?var\(--turbo-model-copy-meta-width\)/);
  assert.match(css, /\.b-model-row__actions\s*\{[\s\S]*?display:\s*grid/);
  assert.match(css, /\.b-popover--wide \.b-action-feedback\.sr-only\s*\{[\s\S]*?clip-path:\s*inset\(50%\)/);
  assert.match(css, /body\[data-active-tab="config"\]\[data-config-view="catalog"\] #panel-config\s*\{[\s\S]*?display:\s*flex;[\s\S]*?overflow:\s*hidden;/);
  assert.match(css, /body\[data-active-tab="config"\]\[data-config-view="catalog"\] #panel-config \.b-stage\s*\{[\s\S]*?flex:\s*1 1 auto;[\s\S]*?min-block-size:\s*0;[\s\S]*?overflow-y:\s*auto;[\s\S]*?overflow-x:\s*hidden;/);
  assert.match(css, /body\[data-active-tab="config"\]\[data-config-view="catalog"\] #panel-config \.b-config-page\s*\{[\s\S]*?display:\s*flex;[\s\S]*?min-block-size:\s*0;/);
  assert.match(css, /body\[data-active-tab="config"\]\[data-config-view="catalog"\] #panel-config \.b-config-view:not\(\[hidden\]\)\s*\{[\s\S]*?display:\s*flex;[\s\S]*?flex:\s*0 1 auto;[\s\S]*?flex-direction:\s*column;/);
  assert.match(css, /body\[data-active-tab="config"\]\[data-config-view="catalog"\] #panel-config \.b-model-catalog\s*\{[\s\S]*?display:\s*flex;[\s\S]*?flex:\s*0 1 auto;[\s\S]*?flex-direction:\s*column;[\s\S]*?gap:\s*0;[\s\S]*?max-block-size:\s*100%;[\s\S]*?min-block-size:\s*0;/);
  assert.match(css, /body\[data-active-tab="config"\]\[data-config-view="catalog"\] #panel-config \.b-model-catalog__list\s*\{[\s\S]*?flex:\s*0 1 auto;[\s\S]*?block-size:\s*fit-content;[\s\S]*?min-block-size:\s*0;[\s\S]*?max-block-size:\s*calc\(70px \* 4 \+ 7px \* 3 \+ 22px\);[\s\S]*?align-content:\s*start;[\s\S]*?overflow-x:\s*hidden;[\s\S]*?overflow-y:\s*auto;[\s\S]*?overscroll-behavior:\s*contain;[\s\S]*?scrollbar-gutter:\s*stable;/);
  assert.match(css, /@media \(min-width: 721px\) and \(max-width: 900px\)\s*\{[\s\S]*?body\[data-active-tab="config"\]\[data-config-view="catalog"\] #panel-config \.b-model-row\s*\{[\s\S]*?min-block-size:\s*92px;/);
  assert.match(css, /@media \(min-width: 721px\) and \(max-width: 900px\)\s*\{[\s\S]*?\.b-model-catalog__list\s*\{[\s\S]*?max-block-size:\s*calc\(92px \* 4 \+ 7px \* 3 \+ 22px\);/);
  assert.match(css, /@media \(max-width: 720px\)\s*\{[\s\S]*?\.b-model-catalog__list\s*\{[\s\S]*?max-block-size:\s*calc\(97px \* 8 \+ 7px \* 7 \+ 22px\);/);
  assert.match(css, /\.b-model-catalog--wide\s*\{[\s\S]*?width:\s*auto;/);
  assert.match(css, /\.b-model-row__drag:hover[\s\S]*?color:\s*var\(--b-accent\);/);
  assert.match(css, /\.b-model-row\.is-dragging\s*\{[\s\S]*?transform:\s*translateY\(var\(--turbo-model-drag-offset\)\) scale\(var\(--turbo-model-drag-scale\)\)[\s\S]*?box-shadow:/);
  assert.match(css, /\.b-model-row\.is-drop-target\s*\{[\s\S]*?border-color:\s*var\(--b-accent\)[\s\S]*?transform:\s*scale\(var\(--turbo-model-drop-scale\)\)[\s\S]*?box-shadow:/);
  assert.match(css, /\.b-model-row\.is-dragging::after[\s\S]*?border:\s*1px dashed/);
  assert.match(css, /\.b-model-row\.is-drop-target::after[\s\S]*?border:\s*2px dashed var\(--b-accent\)/);
  assert.doesNotMatch(css, /\.b-model-catalog__list\s*\{[^}]*overflow:\s*visible;/);
  assert.match(css, /\.b-model-editor__efforts\s*\{[\s\S]*?grid-template-columns:\s*repeat\(6, max-content\);[\s\S]*?justify-content:\s*space-between;/);
  assert.match(css, /\.b-model-editor__efforts label\s*\{[\s\S]*?font-size:\s*0\.62rem;[\s\S]*?white-space:\s*nowrap;/);
  assert.match(css, /\.b-model-editor__efforts label\s*\{[\s\S]*?min-width:\s*max-content;/);
  assert.match(css, /\.b-model-dialog__surface\s*\{[\s\S]*?block-size:\s*min\(760px, calc\(100dvh - 28px\)\);[\s\S]*?grid-template-rows:\s*auto minmax\(0, 1fr\) auto auto;/);
  assert.match(css, /\.b-model-discovery__list\s*\{[\s\S]*?min-height:\s*0;[\s\S]*?max-height:\s*none;[\s\S]*?overflow-y:\s*auto;/);
  assert.match(css, /\.b-transport-toggle\s*\{[\s\S]*?border:\s*1px solid var\(--b-line\);/);
  assert.match(css, /\.b-transport-toggle__option\[aria-pressed="true"\]\s*\{[\s\S]*?color:\s*var\(--b-accent\);[\s\S]*?background:\s*var\(--b-accent-soft\);/);
  assert.match(css, /\.b-transport-toggle__option:focus-visible\s*\{[\s\S]*?outline:\s*2px solid var\(--b-accent\);/);
  assert.match(rust, /get_model_catalog/);
  assert.match(rust, /update_model_catalog/);
});

test("模型目录支持上游发现、编辑向导与完整能力字段", async () => {
  // Given: 模型候选工作区的产品契约。
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const css = await readFile(new URL("styles.css", sourceUrl), "utf8");
  const app = await readFile(new URL("app.js", sourceUrl), "utf8");
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

  // When: 检查前端和原生命令是否暴露完整闭环。
  // Then: 用户能发现、创建、编辑、复制模型，并配置上下文与 reasoning。
  assert.match(html, /data-action="discover-models"/);
  assert.match(html, /data-model-discovery-filter/);
  assert.match(css, /\.b-model-discovery__filter/);
  assert.match(html, /data-action="import-all-models"/);
  assert.match(html, /data-model-editor/);
  assert.match(html, /data-model-field="context-window"/);
  assert.match(html, /data-model-field="max-context-window"/);
  assert.match(html, /data-model-field="reasoning-effort"/);
  assert.match(html, /<legend>思考<\/legend>/);
  assert.match(html, /data-model-efforts/);
  assert.match(html, /data-model-editor-action="save">保存/);
  assert.match(html, /data-model-editor-action="undo">撤销修改/);
  assert.match(html, /class="b-model-editor__identity"/);
  assert.match(html, /class="b-model-editor__advanced"/);
  assert.doesNotMatch(html, /data-model-field="visibility"|data-model-field="effective-context-percent"|data-model-field="truncation-policy"|data-model-field="reasoning-summary"|data-model-field="supports-summary"/);
  assert.doesNotMatch(html, /data-model-field="context-window-number"/);
  assert.doesNotMatch(html, /<legend>输入与工具<\/legend>|<legend>服务与协议<\/legend>/);
  assert.match(app, /discover_model_catalog/);
  assert.match(app, /MODEL_REASONING_OPTIONS/);
  assert.match(app, /MODEL_CONTEXT_DEFAULT = 275_000/);
  assert.match(app, /MODEL_REASONING_OPTIONS = \["low", "medium", "high", "xhigh", "max", "ultra"\]/);
  assert.match(app, /function renderEditorEffortOptions[\s\S]*MODEL_REASONING_OPTIONS\.map/);
  assert.match(app, /supportedReasoningLevels: \[\{ effort: "low", description: "" \}, \{ effort: "medium", description: "" \}, \{ effort: "high", description: "" \}, \{ effort: "xhigh", description: "" \}\]/);
  assert.match(app, /defaultReasoningLevel: "high"/);
  assert.match(app, /effectiveContextWindowPercent: 95/);
  assert.match(app, /truncationPolicy: "auto"/);
  assert.match(app, /inputModalities: \["text", "image"\]/);
  assert.match(app, /supportsSearchTool: false/);
  assert.match(app, /supportsParallelToolCalls: false/);
  assert.match(app, /useResponsesLite: false/);
  assert.match(app, /sourceMarkup = source \?/);
  assert.match(app, /updates = catalogModels\(\)\.map\(\(model, priority\) => \(\{ slug: model\.slug, visibility: model\.visibility, priority: priority \+ 1 \}\)\)/);
  assert.match(app, /persistModelEntry[\s\S]*save_model_settings/);
  assert.doesNotMatch(app, /catalogFullDirty|const saveFull/);
  const catalogMarkup = app.slice(app.indexOf("function modelCatalogMarkup"), app.indexOf("function catalogRows"));
  assert.doesNotMatch(catalogMarkup, /model\.description|modelReasoningLabel|模板/);
  assert.doesNotMatch(catalogMarkup, /b-model-row__priority/);
  assert.match(app, /data-model-source/);
  assert.match(rust, /discover_model_catalog/);
  assert.match(rust, /CatalogModelUpdate/);
});

test("模型发现支持单项导入并保护已存在模型", async () => {
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const app = await readFile(new URL("app.js", sourceUrl), "utf8");

  assert.match(app, /importDiscoveredModel\(/);
  assert.match(app, /data-model-import=/);
  assert.match(app, /escapeHtml\(model\.slug\)/);
  assert.match(app, /disabled data-existing="true"/);
  assert.match(app, /importableCount/);
  assert.match(html, /disabled aria-disabled="true" data-action="import-all-models"/);
  assert.match(app, /editorTouchedFields/);
  assert.match(app, /result\.fieldSources\[field\] = changed/);
});

test("模型编辑器收敛为必需字段与高级配置", async () => {
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  assert.match(html, /data-model-field="slug"/);
  assert.match(html, /data-model-field="displayName"/);
  assert.match(html, /data-model-field="max-context-window"/);
  assert.match(html, /data-model-field="context-window" type="range"/);
  assert.doesNotMatch(html, /data-model-field="context-window-number"/);
  assert.match(html, /data-model-efforts/);
  assert.match(html, /data-model-field="description"/);
  assert.match(html, /data-model-field="transport"/);
});

test("模型请求验证必须命中重启后的目标模型", async () => {
  const runtime = await readFile(new URL("../src-tauri/src/runtime.rs", import.meta.url), "utf8");
  const traffic = await readFile(new URL("../src-tauri/src/proxy/traffic.rs", import.meta.url), "utf8");

  assert.match(runtime, /pending_verification_models/);
  assert.match(runtime, /is_successful_responses_for\(/);
  assert.match(traffic, /pub\(crate\) fn is_successful_responses_for/);
});

test("Windows 重启 Codex 不闪出 PowerShell 并返回新进程", async () => {
  const rust = (await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8")).replaceAll("\r\n", "\n");
  const start = rust.indexOf('#[cfg(target_os = "windows")]\nfn restart_codex_desktop');
  const end = rust.indexOf('#[cfg(not(any(target_os = "macos", target_os = "windows")))]', start);
  const windowsRestart = rust.slice(start, end);

  assert.match(windowsRestart, /crate::windows_process::hidden_command\("powershell\.exe"\)/);
  assert.match(windowsRestart, /Start-Process -FilePath \$path -PassThru/);
  assert.match(windowsRestart, /\.output\(\)/);
  assert.match(windowsRestart, /parse::<u32>\(\)/);
});

test("更新下载失败后进入可重试状态", async () => {
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

  assert.match(
    rust,
    /Err\(error\)\s*=>\s*\{\s*runtime\.set_update_status\(\s*"error",\s*&format!\("下载失败，可重新下载：\{error\}"\),\s*0,?\s*\);/s,
  );
});

test("检查更新失败后不会遗留在检查中", async () => {
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

  assert.match(
    rust,
    /Err\(error\)\s*=>\s*\{\s*runtime\.set_update_status\(\s*"error",\s*&format!\("检查更新失败，可重试：\{error\}"\),\s*0,?\s*\);/s,
  );
});

test("窄屏版本栏仍显示完整更新结果", async () => {
  const css = await readFile(new URL("styles.css", sourceUrl), "utf8");

  assert.doesNotMatch(css, /\.b-version-bar__status > span\s*\{[^}]*display:\s*none;/s);
});

test("顶部 Tab 使用单一滑动指示器表达当前位置", async () => {
  const css = await readFile(new URL("styles.css", sourceUrl), "utf8");

  assert.match(css, /\.turbo-tabs::before/);
  assert.match(css, /body\[data-active-tab="statistics"\] \.turbo-tabs::before/);
  assert.match(css, /body\[data-active-tab="config"\] \.turbo-tabs::before/);
  assert.match(css, /transform var\(--speed-standard\) var\(--ease-out\)/);
  assert.match(css, /@media \(prefers-reduced-motion: reduce\)/);
});

test("配置主状态用最多五条 Strands 表达验证进度", async () => {
  // Given: 配置页主状态和原生 Strands 组件。
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const app = await readFile(new URL("app.js", sourceUrl), "utf8");
  const strands = await readFile(new URL("strands.js", sourceUrl), "utf8");
  const livePanel = html.slice(html.indexOf('id="panel-live"'), html.indexOf('id="panel-statistics"'));
  const configPanel = html.slice(html.indexOf('id="panel-config"'));

  // When: 用户查看已接管状态。
  // Then: 左侧旧图标已删除，右侧动画只接受 0–5 条状态进度。
  assert.doesNotMatch(configPanel, /class="status-orb"/);
  assert.match(configPanel, /data-strands/);
  assert.match(livePanel, /data-strands/);
  assert.equal(html.match(/data-strands/g)?.length, 2);
  assert.match(html, /<script src="\.\/strands\.js"><\/script>/);
  assert.match(app, /TurboStrands\?\.setCount/);
  assert.match(strands, /const MAX_STRANDS = 5/);
  assert.equal(configPanel.match(/class="b-control__icon"/g)?.length, 4);
  assert.equal(configPanel.match(/class="b-control__icon"[^>]*><svg/g)?.length, 4);
});

test("实时 Strands 打开紧凑的非模态 WebSocket 连接检查器", async () => {
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const app = await readFile(new URL("app.js", sourceUrl), "utf8");
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  const livePanel = html.slice(html.indexOf('id="panel-live"'), html.indexOf('id="panel-statistics"'));

  assert.match(livePanel, /data-action="toggle-connections"[\s\S]*aria-controls="connection-inspector"/);
  assert.match(livePanel, /data-connection-dock[\s\S]*id="connection-inspector"[\s\S]*data-connection-summary-trigger[^>]*data-connection-grip/);
  assert.equal(livePanel.match(/data-connection-summary="(?:up|down|idle)"/g)?.length, 3);
  assert.match(app, /点击或上拉收起连接检查器，左右拖动可移动/);
  assert.doesNotMatch(livePanel, /close-connections|class="c-connection-grip"/);
  assert.match(livePanel, /id="connection-inspector"[\s\S]*aria-modal="false"/);
  assert.doesNotMatch(livePanel, /connection-backdrop|aria-modal="true"/);

  const groups = ["prewarm", "bound", "transitions", "closed"]
    .map((name) => livePanel.indexOf(`data-connection-group="${name}"`));
  assert.ok(groups.every((index) => index >= 0));
  assert.deepEqual(groups, [...groups].sort((left, right) => left - right));
  assert.equal(livePanel.match(/class="c-connection-group__hint"/g)?.length, 4);
  assert.equal(livePanel.match(/class="c-connection-group__hint"[^>]*role="note"/g)?.length, 4);
  assert.match(livePanel, /可立即用于新线程、尚未绑定的空白连接。/);
  assert.match(livePanel, /同一 Codex 线程归为一个会话；箭头表示传输中，Zzz 表示空闲。/);
  assert.match(livePanel, /正在建立新连接，或在断开后恢复绑定。/);
  assert.match(livePanel, /最近 5 分钟内关闭的连接，最多显示 8 条。/);

  assert.match(app, /const RECENT_CLOSED_LIMIT = 8;/);
  assert.match(app, /const recentClosed = snapshot\.recentClosed\.slice\(0, RECENT_CLOSED_LIMIT\);/);
  assert.match(html, /<script src="\.\/connection-dom\.js"><\/script>/);
  assert.match(app, /get_connection_snapshot/);
  assert.match(rust, /async fn get_connection_snapshot/);
});

test("开机自启动保持后台且发布流程收集真实 updater 包", async () => {
  const tauriConfig = JSON.parse(
    await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  );
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  const workflow = await readFile(
    new URL("../.github/workflows/desktop-release.yml", import.meta.url),
    "utf8",
  );

  assert.equal(tauriConfig.app.windows[0].visible, false);
  assert.equal(tauriConfig.app.windows[0].minWidth, 360);
  assert.equal(tauriConfig.app.windows[0].minHeight, tauriConfig.app.windows[0].height);
  assert.match(rust, /args_os\(\)[\s\S]*--background/);
  assert.match(workflow, /TAURI_SIGNING_PRIVATE_KEY_PASSWORD/);
  assert.doesNotMatch(workflow, /test -n "\$TAURI_SIGNING_PRIVATE_KEY_PASSWORD"/);
  assert.match(workflow, /platform: darwin-aarch64[\s\S]*bundles: app,dmg/);
  assert.match(workflow, /platform: windows-x86_64[\s\S]*bundles: nsis/);
  assert.match(
    workflow,
    /npx tauri build --ci --bundles "\$\{\{ matrix\.bundles \}\}" --config tauri-release-config\.json/,
  );
  assert.match(workflow, /node scripts\/desktop-release\.mjs assemble-ci release-inputs desktop-release/);
  assert.match(workflow, /createUpdaterArtifacts: true/);
  assert.match(workflow, /platform: darwin-aarch64[\s\S]*apple_signing_identity: "-"/);
  assert.match(workflow, /APPLE_SIGNING_IDENTITY: \$\{\{ matrix\.apple_signing_identity \}\}/);
  assert.match(workflow, /codesign --verify --deep --strict/);
  assert.match(workflow, /\.exe\.sig/);
  assert.doesNotMatch(workflow, /\.exe\.zip/);
});

test("桌面版本和 updater endpoint 由同一编译期契约驱动", async () => {
  const packageJson = JSON.parse(await readFile(new URL("../package.json", import.meta.url), "utf8"));
  const cargo = await readFile(new URL("../src-tauri/Cargo.toml", import.meta.url), "utf8");
  const tauriConfig = JSON.parse(
    await readFile(new URL("../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
  );
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

  assert.equal(packageJson.version, "0.1.0-pre.1");
  assert.match(cargo, /^version = "0\.1\.0-pre\.1"$/m);
  assert.equal(tauriConfig.version, "0.1.0-pre.1");
  assert.equal(packageJson.scripts["desktop:release:local"], "node scripts/desktop-release.mjs");
  assert.match(rust, /option_env!\("TURBO_UPDATER_ENDPOINT"\)/);
  assert.match(rust, /https:\/\/ai-cove\.com\/downloads\/turbo\/latest\.json/);
});

test("macOS 状态栏使用紧凑的 Turbo 模板剪影", async () => {
  // Given: Turbo 使用原生托盘承载后台入口。
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

  // When: 应用创建右上角状态栏项目。
  const traySetup = rust.slice(rust.indexOf("fn install_tray"), rust.indexOf("fn initialize_desktop_preferences"));

  // Then: 使用稳定 ID 和系统模板剪影；完整应用图标只留给非 macOS。
  assert.match(rust, /const TRAY_ID: &str = "ai-cove-turbo"/);
  assert.match(traySetup, /TrayIconBuilder::with_id\(TRAY_ID\)/);
  assert.match(traySetup, /include_bytes!\(\s*"\.\.\/icons\/tray-template\.png"\s*\)/);
  assert.match(traySetup, /#\[cfg\(target_os = "macos"\)\][\s\S]*\.icon_as_template\(true\)/);
  assert.doesNotMatch(traySetup, /\.title\("T"\)/);
  assert.match(traySetup, /#\[cfg\(not\(target_os = "macos"\)\)\][\s\S]*default_window_icon\(\)/);
});

test("macOS Dock 点击会重新显示被隐藏的主窗口", async () => {
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

  assert.match(
    rust,
    /RunEvent::Reopen \{ \.\. \} => show_main_window\(app_handle\)/,
    "Dock reopen 事件必须恢复主窗口",
  );
});

test("macOS 升级后首次显示 Dock 且后续尊重用户选择", async () => {
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  const runtime = await readFile(new URL("../src-tauri/src/runtime.rs", import.meta.url), "utf8");

  assert.match(rust, /if !runtime\.dock_initialized\(\) \{\s*runtime\.set_dock_state\(true\);\s*\}/s);
  assert.match(runtime, /dock_visible:\s*true/);
  assert.match(runtime, /dock_initialized:\s*false/);
  assert.match(runtime, /preferences\.dock_visible = visible;\s*preferences\.dock_initialized = true;/s);
});
