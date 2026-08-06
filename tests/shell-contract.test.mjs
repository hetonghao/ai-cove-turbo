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
  assert.doesNotMatch(configPanel.slice(configCardIndex), /data-action="check-for-updates"|data-state="update-state"/);
  assert.doesNotMatch(html, /00:09:42|STREAMING/);
  assert.match(`${html}\n${css}\n${app}`, /扩展由上游协商/);
  assert.doesNotMatch(`${html}\n${css}\n${app}`, /permessage-deflate/i);
  assert.doesNotMatch(livePanel, /data-action="toggle-(compression|websocket|autostart|dock)"/);
  assert.doesNotMatch(statisticsPanel, /data-action=/);
  assert.match(livePanel, /data-live-recovery/);
  assert.match(app, /handleChartKeydown/);
  assert.doesNotMatch(livePanel, /c-topbar|AI COVE TURBO/);
  assert.doesNotMatch(statisticsPanel, /c-topbar|AI COVE TURBO/);
  assert.doesNotMatch(configPanel, /b-header|turbo-icon--popover/);
  assert.doesNotMatch(css, /\.a-|variant--a|turbo-variant-switcher|app-shell|data-variant/);
});

test("Tauri 前端通过约定命令读取和修改真实状态", async () => {
  // Given: Rust 后端暴露的桌面命令契约。
  const app = await readFile(new URL("app.js", sourceUrl), "utf8");
  const commands = [
    "get_app_status",
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
  assert.match(rust, /args_os\(\)[\s\S]*--background/);
  assert.match(workflow, /TAURI_SIGNING_PRIVATE_KEY_PASSWORD/);
  assert.match(workflow, /test -n "\$TAURI_SIGNING_PRIVATE_KEY_PASSWORD"/);
  assert.match(workflow, /\.exe\.zip/);
  assert.match(workflow, /\.exe\.zip\.sig/);
});

test("macOS 状态栏始终显示 Turbo 标识", async () => {
  // Given: Turbo 使用原生托盘承载后台入口。
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");

  // When: 应用创建右上角状态栏项目。
  const traySetup = rust.slice(rust.indexOf("fn install_tray"), rust.indexOf("fn initialize_desktop_preferences"));

  // Then: 除图标外还提供固定短标题，避免状态栏项目不可见。
  assert.match(traySetup, /\.title\("Turbo"\)/);
});
