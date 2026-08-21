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
  const statElements = ["requests", "raw-bytes", "sent-bytes", "saved-bytes", "savings-rate", "speed-gain"]
    .map((stat) => element({ dataset: { stat } }));
  const requestStream = element();
  const streamEmpty = element();
  const liveCount = element();
  const bars = element();
  const chartSlots = Array.from({ length: 6 }, (_, index) => element({
    tabIndex: index === 0 ? 0 : -1,
    focus() { this.focused = true; },
  }));
  let barMarkup = "";
  let barWrites = 0;
  Object.defineProperty(bars, "innerHTML", {
    get() { return barMarkup; },
    set(value) { barMarkup = value; barWrites += 1; },
  });
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
      { id: 1, timestampMs: 70_000, status: 201, path: "/v1/direct", rawBytes: 200, sentBytes: 100, transport: "HTTP", result: "success", route: "directHttp" },
      { id: 2, timestampMs: 71_000, status: 101, path: "/v1/hybrid-ws", rawBytes: 100, sentBytes: 50, transport: "WS", result: "success", route: "hybridWs" },
      { id: 3, timestampMs: 72_000, status: 101, path: "/v1/ws-error", rawBytes: 100, sentBytes: 50, transport: "WS", result: "error", route: "hybridWs" },
      { id: 4, timestampMs: 73_000, status: 200, path: "/v1/cold-start", rawBytes: 100, sentBytes: 50, transport: "HTTP", result: "success", route: "hybridColdStartHttp" },
      { id: 5, timestampMs: 74_000, status: 200, path: "/v1/recovery", rawBytes: 100, sentBytes: 50, transport: "HTTP", result: "fallback", route: "hybridRecoveryHttp" },
      { id: 6, timestampMs: 75_000, status: 1002, path: "/v1/ws-idle", rawBytes: 0, sentBytes: 0, transport: "WS", result: "error", route: "hybridWs", failurePhase: "hybridIdle", failureReason: "unexpected idle upstream binary message" },
      { id: 7, timestampMs: 76_000, status: 1012, path: "/v1/ws-restart", rawBytes: 0, sentBytes: 0, transport: "WS", result: "error", route: "hybridWs", failurePhase: "hybridIdle", failureReason: "service restarting" },
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
    addEventListener(type, handler) { listeners[type] = handler; },
    querySelector(selector) {
      return selectors.get(selector) ?? null;
    },
    querySelectorAll(selector) {
      if (selector === "[data-tab]") return tabs;
      if (selector === "[data-panel]") return panels;
      if (selector === "[data-stat]") return statElements;
      if (selector === "[data-live-count]") return [liveCount];
      if (selector === "[data-stat-bars] .c-bar-slot") return chartSlots;
      return [];
    },
  };
  const listeners = {};
  let onTick;
  const window = {
    __TAURI__: { core: { invoke: async () => status } },
    location: { href: "tauri://localhost/?tab=statistics" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval(handler) { onTick = handler; },
  };
  const context = { document, Error, Intl, Math, Number, Object, Set, URL, window };

  // When: 正式页面读取一次真实状态。
  vm.runInNewContext(telemetrySource, context);
  vm.runInNewContext(appSource, context);
  await new Promise((resolve) => setImmediate(resolve));

  // Then: 统计页只渲染统计视图，切换到实时页后再从同一份状态数据补齐终端。
  assert.equal(requestStream.innerHTML, "");
  listeners.click({
    target: {
      closest(selector) {
        if (selector === "[data-tab]") return tabs[0];
        return null;
      },
    },
  });
  const requestRows = requestStream.innerHTML.match(/<tr\b.*?<\/tr>/g) ?? [];
  const failedRow = requestRows.find((row) => row.includes("/v1/ws-error")) ?? "";
  const recoveredRow = requestRows.find((row) => row.includes("/v1/ws-idle")) ?? "";
  const restartRow = requestRows.find((row) => row.includes("/v1/ws-restart")) ?? "";
  assert.equal(requestRows.length, 7);
  assert.match(requestRows.find((row) => row.includes("/v1/direct")) ?? "", />压缩 HTTP<\/span>/);
  assert.match(requestRows.find((row) => row.includes("/v1/hybrid-ws")) ?? "", />Hybrid WS<\/span>/);
  assert.match(requestRows.find((row) => row.includes("/v1/cold-start")) ?? "", />首轮 HTTP<\/span>/);
  assert.match(requestRows.find((row) => row.includes("/v1/recovery")) ?? "", />回退 HTTP<\/span>/);
  assert.match(failedRow, /c-request-status c-request-status--error">101<\/span>/);
  assert.match(failedRow, /c-transport c-transport--error">Hybrid WS · 请求失败<\/span>/);
  assert.doesNotMatch(failedRow, /c-request-status--success|<span class="c-transport">Hybrid WS<\/span>|Hybrid WS · 失败/);
  assert.match(recoveredRow, /title="unexpected idle upstream binary message"/);
  assert.match(recoveredRow, />Hybrid WS · 连接恢复<\/span>/);
  assert.doesNotMatch(recoveredRow, /Hybrid WS · 失败/);
  assert.match(restartRow, />Hybrid WS · 发布重建<\/span>/);
  assert.equal(liveCount.textContent, "7");
  assert.equal(statElements[0].textContent, "5");
  assert.equal(statElements[1].textContent, "300 B");
  assert.equal(statElements[2].textContent, "150 B");
  assert.equal(statElements[3].textContent, "150 B");
  assert.equal(statElements[4].textContent, "50.0%");
  assert.equal(statElements[5].textContent, "25.0% / 9.6%");
  assert.equal((bars.innerHTML.match(/class="c-bar-slot/g) ?? []).length, 6);
  assert.equal(chartSlots.filter((slot) => slot.tabIndex === 0).length, 1);
  assert.match(bars.innerHTML, /class="c-bar-slot" style="--bar: 100%/);
  assert.equal(granularity.textContent, "每 10 秒");
  assert.equal(statEmpty.hidden, true);
  assert.doesNotMatch(windows.innerHTML, /提速约|基准模型估算/);

  // When: 键盘用户在图表中向右浏览一个时间桶。
  let prevented = false;
  listeners.keydown({
    key: "ArrowRight",
    preventDefault() { prevented = true; },
    target: { closest(selector) { return selector === ".c-bar-slot" ? chartSlots[0] : null; } },
  });

  // Then: 只有新的时间桶进入 Tab 序列，焦点直接跟随。
  assert.equal(prevented, true);
  assert.equal(chartSlots[0].tabIndex, -1);
  assert.equal(chartSlots[1].tabIndex, 0);
  assert.equal(chartSlots[1].focused, true);

  // When: 下一次轮询返回完全相同的统计桶。
  const writesAfterFirstStatus = barWrites;
  await onTick();

  // Then: 柱状图节点不被重建，悬停提示不会按轮询频率闪烁。
  assert.equal(barWrites, writesAfterFirstStatus);
});

test("速度估算按压缩字节和已就绪 WebSocket 请求加权", async () => {
  // Given: 一个长 HTTP 压缩请求和一个长 WebSocket 压缩请求。
  const source = await readFile(new URL("../src/telemetry.js", import.meta.url), "utf8");
  const window = {};
  vm.runInNewContext(source, { Intl, Math, Number, Object, window });
  const buckets = [{
    series: [
      { transport: "HTTP", result: "success", requests: 1, rawBytes: 108_257, sentBytes: 23_403 },
      { transport: "WS", result: "success", requests: 1, rawBytes: 108_268, sentBytes: 23_421 },
      { transport: "WS", result: "error", requests: 3, rawBytes: 300, sentBytes: 150 },
    ],
  }];

  // When: 汇总当前筛选范围的速度提升。
  const estimate = window.TurboTelemetry.estimateSpeed(buckets, "all", "all");

  // Then: 错误请求不进入提速分母，WS 请求额外获得连接复用常量。
  assert.equal(estimate.requests, 2);
  assert.equal(estimate.firstPercent.toFixed(1), "19.7");
  assert.equal(estimate.completePercent.toFixed(1), "9.0");
  assert.equal(window.TurboTelemetry.formatSpeedGain(estimate), "19.7% / 9.0%");
});
