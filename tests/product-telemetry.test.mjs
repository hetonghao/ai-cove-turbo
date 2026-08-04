import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

function element(extra = {}) {
  return {
    dataset: {},
    hidden: false,
    innerHTML: "",
    style: { setProperty() {} },
    textContent: "",
    setAttribute() {},
    ...extra,
  };
}

test("正式前端用 Tauri 业务数据渲染实时终端，且错误结果覆盖 101 状态", async () => {
  // Given: get_app_status 返回成功和失败的 101 请求，以及固定六桶的一分钟窗口。
  const telemetrySource = await readFile(new URL("../src/telemetry.js", import.meta.url), "utf8");
  const appSource = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const statElements = ["requests", "raw-bytes", "sent-bytes", "saved-bytes", "savings-rate"]
    .map((stat) => element({ dataset: { stat } }));
  const requestStream = element();
  const streamEmpty = element();
  const liveCount = element();
  const bars = element();
  const axis = element();
  const granularity = element();
  const windows = element();
  const statEmpty = element();
  const summary = element();
  const range = element({ value: "1" });
  const transport = element({ value: "all" });
  const result = element({ value: "all" });
  const tabs = ["live", "statistics", "config"].map((tab) => element({ dataset: { tab }, focus() {} }));
  const panels = ["live", "statistics", "config"].map((panel) => element({ dataset: { panel } }));
  const buckets = Array.from({ length: 6 }, (_, index) => ({
    startMs: 20_000 + index * 10_000,
    endMs: 30_000 + index * 10_000,
    series: index === 0
      ? [{ transport: "WS", result: "success", requests: 4, rawBytes: 100, sentBytes: 50 }]
      : index === 5
        ? [{ transport: "HTTP", result: "success", requests: 1, rawBytes: 200, sentBytes: 100 }]
        : [],
  }));
  const status = {
    serviceHealthy: true,
    configState: "managed",
    recentRequests: [
      { id: 1, timestampMs: 70_000, status: 201, path: "/v1/responses", rawBytes: 200, sentBytes: 100, transport: "HTTP", result: "success" },
      { id: 2, timestampMs: 71_000, status: 101, path: "/v1/responses", rawBytes: 100, sentBytes: 50, transport: "WS", result: "success" },
      { id: 3, timestampMs: 72_000, status: 101, path: "/v1/ws-error", rawBytes: 100, sentBytes: 50, transport: "WS", result: "error" },
    ],
    trafficWindows: [
      { minutes: 1, bucketSeconds: 10, currentPeriodStartMs: 70_000, buckets },
      { minutes: 10, bucketSeconds: 60, currentPeriodStartMs: 60_000, buckets: [] },
      { minutes: 60, bucketSeconds: 300, currentPeriodStartMs: 0, buckets: [] },
      { minutes: 1440, bucketSeconds: 3600, currentPeriodStartMs: 0, buckets: [] },
    ],
  };
  const selectors = new Map([
    ['[data-filter="range"]', range],
    ['[data-filter="transport"]', transport],
    ['[data-filter="result"]', result],
    ["[data-request-stream]", requestStream],
    ["[data-stream-empty]", streamEmpty],
    ["[data-stat-bars]", bars],
    ["[data-stat-axis]", axis],
    ["[data-stat-granularity]", granularity],
    ["[data-stat-windows]", windows],
    ["[data-stat-empty]", statEmpty],
    ["[data-stats-summary]", summary],
  ]);
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelector(selector) {
      return selectors.get(selector) ?? null;
    },
    querySelectorAll(selector) {
      if (selector === "[data-tab]") return tabs;
      if (selector === "[data-panel]") return panels;
      if (selector === "[data-stat]") return statElements;
      if (selector === "[data-live-count]") return [liveCount];
      return [];
    },
  };
  const window = {
    __TAURI__: { core: { invoke: async () => status } },
    location: { href: "tauri://localhost/?tab=statistics" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval() {},
  };
  const context = { document, Error, Intl, Math, Number, Object, Set, URL, window };

  // When: 正式页面读取一次真实状态。
  vm.runInNewContext(telemetrySource, context);
  vm.runInNewContext(appSource, context);
  await new Promise((resolve) => setImmediate(resolve));

  // Then: 终端与统计图都来自同一份状态数据。
  const requestRows = requestStream.innerHTML.match(/<tr>.*?<\/tr>/g) ?? [];
  const failedRow = requestRows.find((row) => row.includes("/v1/ws-error")) ?? "";
  assert.equal(requestRows.length, 3);
  assert.match(requestStream.innerHTML, /201[\s\S]*\/v1\/responses[\s\S]*HTTP/);
  assert.match(failedRow, /c-request-status c-request-status--error">101<\/span>/);
  assert.match(failedRow, /c-transport c-transport--error">WS · 失败<\/span>/);
  assert.doesNotMatch(failedRow, /c-request-status--success|<span class="c-transport">WS<\/span>/);
  assert.equal(liveCount.textContent, "3");
  assert.equal(statElements[0].textContent, "5");
  assert.equal(statElements[1].textContent, "300 B");
  assert.equal(statElements[2].textContent, "150 B");
  assert.equal(statElements[3].textContent, "150 B");
  assert.equal(statElements[4].textContent, "50.0%");
  assert.equal((bars.innerHTML.match(/class="c-bar-slot/g) ?? []).length, 6);
  assert.match(bars.innerHTML, /class="c-bar-slot" style="--bar: 100%/);
  assert.equal(granularity.textContent, "每 10 秒");
  assert.equal(statEmpty.hidden, true);
});
