import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

function element(dataset = {}) {
  return {
    dataset,
    hidden: false,
    style: { setProperty() {} },
    setAttribute() {},
    focus() {},
  };
}

async function runApp(source, context) {
  const telemetrySource = await readFile(new URL("../src/telemetry.js", import.meta.url), "utf8");
  vm.runInNewContext(telemetrySource, context);
  vm.runInNewContext(source, context);
}

test("Tauri 首帧在真实状态返回前保持未验证", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const states = [
    element({ state: "service-title" }),
    element({ state: "websocket-status" }),
    element({ state: "requests" }),
  ];
  const tabs = [element({ tab: "config" }), element({ tab: "runtime" })];
  const panels = [element({ panel: "config" }), element({ panel: "runtime" })];
  const invoked = [];
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelectorAll(selector) {
      if (selector === "[data-state]") return states;
      if (selector === "[data-tab]") return tabs;
      if (selector === "[data-panel]") return panels;
      return [];
    },
  };
  const window = {
    __TAURI__: { core: { invoke: (command) => { invoked.push(command); return new Promise(() => {}); } } },
    location: { href: "tauri://localhost/?tab=config" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval() {},
  };

  await runApp(source, { document, window, URL, Intl });

  assert.equal(states[0].textContent, "正在读取状态");
  assert.equal(states[1].textContent, "等待首次握手验证");
  assert.equal(states[2].textContent, "0");
  assert.deepEqual(invoked, ["get_app_status"]);
});

test("非 AI Cove 上游接管后仍持续显示兼容性警告", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const warning = element({ visible: "non-ai-cove" });
  const confirm = element({ visible: "confirm-non-ai-cove", action: "confirm-non-ai-cove" });
  const tabs = [element({ tab: "config" }), element({ tab: "runtime" })];
  const panels = [element({ panel: "config" }), element({ panel: "runtime" })];
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelectorAll(selector) {
      if (selector === "[data-visible]") return [warning, confirm];
      if (selector === "[data-tab]") return tabs;
      if (selector === "[data-panel]") return panels;
      return [];
    },
  };
  const window = {
    __TAURI__: {
      core: {
        invoke: async () => ({
          configState: "managed",
          serviceHealthy: true,
          aiCoveUpstream: false,
          upstream: "https://example.com/v1",
        }),
      },
    },
    location: { href: "tauri://localhost/?tab=config" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval() {},
  };

  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(warning.hidden, false);
  assert.equal(confirm.hidden, true);
});

test("本机回环上游显示 AI Cove 修复入口而不是通用重试", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const genericWarning = element({ visible: "non-ai-cove" });
  const repair = element({ visible: "ai-cove-upstream" });
  const retry = element({ visible: "retry" });
  const tabs = [element({ tab: "config" }), element({ tab: "runtime" })];
  const panels = [element({ panel: "config" }), element({ panel: "runtime" })];
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelectorAll(selector) {
      if (selector === "[data-visible]") return [genericWarning, repair, retry];
      if (selector === "[data-tab]") return tabs;
      if (selector === "[data-panel]") return panels;
      return [];
    },
  };
  const window = {
    __TAURI__: {
      core: {
        invoke: async () => ({
          configState: "blocked",
          aiCoveUpstream: false,
          aiCoveUpstreamFixAvailable: true,
          upstream: "—",
        }),
      },
    },
    location: { href: "tauri://localhost/?tab=config" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval() {},
  };

  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(genericWarning.hidden, true);
  assert.equal(repair.hidden, false);
  assert.equal(retry.hidden, true);
});
