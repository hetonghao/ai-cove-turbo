import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const sourceUrl = new URL("../src/", import.meta.url);

test("桌面壳提供独立 WebSocket 开关与只读运行指标", async () => {
  // Given: Turbo 的生产前端入口。
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const css = await readFile(new URL("styles.css", sourceUrl), "utf8");
  const app = await readFile(new URL("app.js", sourceUrl), "utf8");

  // When: 用户打开设置窗口。
  const tabs = html.match(/role="tab"/g) ?? [];
  const runtimePanel = html.slice(html.indexOf('id="panel-runtime"'));

  // Then: 配置与运行页可访问，WebSocket 只在配置页可切换，运行页保持只读。
  assert.equal(tabs.length, 2);
  assert.match(html, /data-tab="config"/);
  assert.match(html, /data-tab="runtime"/);
  assert.match(html, /assets\/turbo-icon\.png/);
  assert.match(html, /data-state="endpoint"/);
  assert.match(html, /data-state="config-message"/);
  assert.match(html, /data-state="provider"/);
  assert.match(html, /data-state="upstream"/);
  assert.match(html, /data-state="compression-verified"/);
  assert.match(html, /data-action="toggle-websocket"/);
  assert.match(html, /data-state="websocket"/);
  assert.match(html, /data-state="websocket-status"/);
  assert.match(html, /data-state="websocket-detail"/);
  assert.match(html, /data-state="websocket-handshakes"/);
  assert.match(html, /data-state="http-fallbacks"/);
  assert.match(html, /data-action="toggle-autostart"/);
  assert.match(html, /data-action="toggle-dock"/);
  assert.match(html, /data-action="restart-codex"/);
  assert.match(html, /data-action="retry-takeover"/);
  assert.match(html, /data-action="set-ai-cove-upstream"/);
  assert.match(html, /data-visible="ai-cove-upstream"/);
  assert.match(html, /data-action="confirm-non-ai-cove"/);
  assert.match(html, /data-action="check-for-updates"/);
  assert.match(html, /data-action="install-update"/);
  assert.doesNotMatch(html, /00:09:42|STREAMING/);
  assert.match(`${html}\n${css}\n${app}`, /扩展由上游协商/);
  assert.doesNotMatch(`${html}\n${css}\n${app}`, /permessage-deflate/i);
  assert.doesNotMatch(runtimePanel, /data-action=/);
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
