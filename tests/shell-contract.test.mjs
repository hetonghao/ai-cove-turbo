import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const sourceUrl = new URL("../src/", import.meta.url);

test("桌面壳提供无 WebSocket 的双 Tab 界面", async () => {
  // Given: Turbo 的生产前端入口。
  const html = await readFile(new URL("index.html", sourceUrl), "utf8");
  const css = await readFile(new URL("styles.css", sourceUrl), "utf8");

  // When: 用户打开设置窗口。
  const tabs = html.match(/role="tab"/g) ?? [];
  const runtimePanel = html.slice(html.indexOf('id="panel-runtime"'));

  // Then: 配置与运行页可访问，运行页只读，且 MVP 不暴露 WebSocket。
  assert.equal(tabs.length, 2);
  assert.match(html, /data-tab="config"/);
  assert.match(html, /data-tab="runtime"/);
  assert.match(html, /assets\/turbo-icon\.png/);
  assert.match(html, /OBSERVED \/ WAITING/);
  assert.doesNotMatch(html, /00:09:42|STREAMING/);
  assert.doesNotMatch(html, /websocket/i);
  assert.doesNotMatch(runtimePanel, /data-action=/);
  assert.doesNotMatch(css, /\.a-|variant--a|turbo-variant-switcher|app-shell|data-variant/);
});
