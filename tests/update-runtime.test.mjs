import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

const sources = await Promise.all(["telemetry.js", "connection-dom.js", "app.js"].map((file) => readFile(new URL("../src/" + file, import.meta.url), "utf8")));
const flush = () => new Promise((resolve) => setImmediate(resolve));

async function updateHarness({ preference = {}, initialState = "available", failCheck = false } = {}) {
  const controls = new Map();
  function element(dataset = {}) {
    return {
      dataset, hidden: false, disabled: false, textContent: "",
      style: { setProperty() {} },
      classList: { add() {}, remove() {}, toggle() {} },
      setAttribute() {}, focus() {},
      closest(selector) { return selector === "[data-action]" && dataset.action ? this : null; },
    };
  }
  const actionSelector = (action) => "[data-action='" + action + "']";
  for (const action of ["install-update", "install-update-bubble", "ignore-update-bubble", "check-for-updates"]) {
    controls.set(actionSelector(action), element({ action }));
  }
  const slot = element();
  const label = element({ state: "update-message" });
  controls.set("[data-update-bubble-slot]", slot);
  controls.set("[data-update-bubble]", element());
  const calls = [];
  let status = { updateState: initialState, updateMessage: "发现新版本 1.1.1", updateProgress: 0 };
  let onClick;
  let onTick;
  let onVisibility;
  let rejectInstall;
  const document = {
    hidden: false, visibilityState: "visible", readyState: "complete", body: element(),
    addEventListener(type, handler) {
      if (type === "click") onClick = handler;
      if (type === "visibilitychange") onVisibility = handler;
    },
    querySelector(selector) { return controls.get(selector) ?? null; },
    querySelectorAll(selector) {
      if (selector === "[data-action]") return [...controls.values()].filter((control) => control.dataset.action);
      if (selector === "[data-state]") return [label];
      return [];
    },
  };
  const window = {
    __TAURI__: { core: { invoke(command) {
      calls.push(command);
      if (command === "install_update") return new Promise((resolve, reject) => { rejectInstall = reject; });
      if (command === "check_for_updates") {
        if (failCheck) return Promise.reject(new Error("offline"));
        status = { updateState: "current", updateMessage: "Turbo 已是最新", updateProgress: 100 };
      }
      return Promise.resolve(command === "get_connection_snapshot" ? {} : status);
    } } },
    localStorage: {
      getItem() { return JSON.stringify(preference); },
      setItem(key, value) { preference = JSON.parse(value); },
    },
    location: { href: "tauri://localhost/?tab=config" },
    history: { replaceState() {} }, matchMedia: () => ({ matches: true }),
    addEventListener() {}, setInterval(handler) { onTick = handler; },
  };
  const context = vm.createContext({ document, window, URL, Intl });
  for (const source of sources) vm.runInContext(source, context);
  await flush();
  return {
    calls, slot, label,
    control(action) { return controls.get(actionSelector(action)); },
    click(action) { onClick({ target: this.control(action) }); },
    tick() { return onTick(); },
    async visible() { onVisibility(); await flush(); },
    setStatus(next) { status = next; },
    failInstall() { rejectInstall(new Error("offline")); },
    preference() { return preference; },
  };
}

test("点击立即更新后气泡保持关闭且轮询不会恢复按钮", async () => {
  const app = await updateHarness({ initialState: "idle" });
  app.setStatus({ updateState: "available", updateMessage: "发现新版本 1.1.2" });
  await app.tick();
  assert.equal(app.slot.hidden, false);

  app.click("install-update-bubble");
  assert.equal(app.slot.hidden, true);
  assert.equal(app.control("install-update").disabled, true);
  assert.equal(app.control("install-update-bubble").disabled, true);
  assert.match(app.label.textContent, /正在准备更新/);
  await app.tick();
  assert.equal(app.slot.hidden, true);
  assert.match(app.label.textContent, /正在准备更新/);
  app.click("install-update-bubble");
  app.click("install-update");
  assert.equal(app.calls.filter((command) => command === "install_update").length, 1);
});

test("安装入口失败后显示更新错误并恢复重试按钮", async () => {
  const app = await updateHarness();
  app.click("install-update");
  app.failInstall();
  await flush();

  assert.equal(app.control("install-update").disabled, false);
  assert.equal(app.control("install-update").textContent, "重新下载");
  assert.match(app.label.textContent, /更新未完成/);
});

test("重启后重新检查真实版本并丢弃当天旧更新结果", async () => {
  const now = new Date();
  const day = [now.getFullYear(), String(now.getMonth() + 1).padStart(2, "0"), String(now.getDate()).padStart(2, "0")].join("-");
  const app = await updateHarness({
    initialState: "idle",
    preference: { checkedDay: day, lastState: "available", lastMessage: "发现新版本 1.1.1", lastProgress: 0 },
  });

  assert.equal(app.calls.filter((command) => command === "check_for_updates").length, 1);
  assert.equal(app.slot.hidden, true);
  assert.equal(app.label.textContent, "Turbo 已是最新");
  await app.tick();
  await app.visible();
  assert.equal(app.calls.filter((command) => command === "check_for_updates").length, 1);
  assert.equal(Object.hasOwn(app.preference(), "lastState"), false);
  assert.equal(Object.hasOwn(app.preference(), "checkedDay"), false);
});

test("更新检查离线时不恢复缓存的新版本提示", async () => {
  const app = await updateHarness({ initialState: "idle", failCheck: true, preference: { lastState: "available", lastMessage: "发现新版本 1.1.1" } });

  assert.equal(app.slot.hidden, true);
  assert.match(app.label.textContent, /检查更新失败/);
});

test("忽略更新后状态轮询和重开页面仍保留当天忽略偏好", async () => {
  const app = await updateHarness();
  app.setStatus({ updateState: "available", updateMessage: "发现新版本 1.1.2" });
  await app.tick();
  app.click("ignore-update-bubble");
  await app.tick();
  assert.equal(app.slot.hidden, true);

  const reopened = await updateHarness({ preference: app.preference() });
  reopened.setStatus({ updateState: "available", updateMessage: "发现新版本 1.1.2" });
  await reopened.tick();
  assert.equal(reopened.slot.hidden, true);
});
