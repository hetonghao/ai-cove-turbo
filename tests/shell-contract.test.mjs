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
  const configCardIndex = configPanel.indexOf('class="b-popover b-popover--wide"');
  const versionBarEndIndex = configPanel.indexOf("</header>", versionBarIndex);
  const updateProgressIndex = configPanel.indexOf('class="b-progress', versionBarIndex);

  // Then: 实时、统计和配置页可访问，业务控制仍只出现在配置页。
  assert.equal(tabs.length, 3);
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
  assert.match(html, /data-action="toggle-autostart"/);
  assert.match(html, /data-action="toggle-dock"/);
  assert.match(html, /data-action="restart-codex"/);
  assert.match(html, /data-action="retry-takeover"/);
  assert.match(html, /data-action="set-ai-cove-upstream"/);
  assert.match(html, /data-visible="ai-cove-upstream"/);
  assert.match(html, /data-action="confirm-non-ai-cove"/);
  assert.match(html, /data-action="check-for-updates"/);
  assert.match(html, /data-action="install-update"/);
  assert.ok(versionBarIndex >= 0 && versionBarIndex < configStageIndex && configStageIndex < configCardIndex);
  assert.ok(configPanel.includes(`>v${packageJson.version}</span>`));
  assert.equal(configPanel.match(/data-action="check-for-updates"/g)?.length, 1);
  assert.equal(configPanel.match(/data-state="update-state"/g)?.length, 1);
  assert.ok(updateProgressIndex > versionBarIndex && updateProgressIndex < versionBarEndIndex);
  assert.match(configPanel, /class="b-version-bar__percent[^>]*data-state="update-progress"/);
  assert.match(css, /\.b-version-bar \.b-progress\s*\{[^}]*position: absolute;/s);
  assert.match(css, /transform: scaleX\(var\(--progress, 0\)\)/);
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
    "check_for_updates",
    "install_update",
  ];

  // When: 前端加载并进入真实桌面运行时。
  // Then: 所有动作走 invoke，状态每秒刷新，浏览器预览仍有明确降级路径。
  for (const command of commands) assert.match(app, new RegExp(`['"]${command}['"]`));
  assert.match(app, /window\.__TAURI__\?\.core\?\.invoke/);
  assert.match(app, /setInterval\([^,]+,\s*1_000\)/s);
  assert.match(app, /Preview/);
  assert.match(app, /enabled/);
  assert.match(app, /visible/);
  assert.match(app, /command === "confirm_non_ai_cove"\) state\.nonAiCoveConfirmed = true/);
  assert.match(app, /"set-ai-cove-upstream": \["set_ai_cove_upstream"\]/);
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

  assert.equal(packageJson.version, "0.1.0-beta.3");
  assert.match(cargo, /^version = "0\.1\.0-beta\.3"$/m);
  assert.equal(tauriConfig.version, "0.1.0-beta.3");
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
