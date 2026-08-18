import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

function element(dataset = {}) {
  const attributes = new Map();
  return {
    attributes,
    dataset,
    hidden: false,
    style: { setProperty() {} },
    setAttribute(name, value) { attributes.set(name, String(value)); },
    focus() {},
  };
}

function motionElement(dataset = {}, hidden = false) {
  const target = element(dataset);
  const animations = [];
  target.hidden = hidden;
  target.animate = (keyframes, options) => {
    const animation = {
      keyframes,
      options,
      cancelled: false,
      cancel() { this.cancelled = true; },
    };
    animations.push(animation);
    return animation;
  };
  target.getAnimations = () => animations;
  target.animations = animations;
  return target;
}

async function runApp(source, context) {
  const telemetrySource = await readFile(new URL("../src/telemetry.js", import.meta.url), "utf8");
  const connectionDomSource = await readFile(new URL("../src/connection-dom.js", import.meta.url), "utf8");
  vm.runInNewContext(telemetrySource, context);
  vm.runInNewContext(connectionDomSource, context);
  vm.runInNewContext(source, context);
}

test("产品图标气泡支持轻触关闭并打开 AI Cove", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const trigger = element({ action: "toggle-ai-cove-bubble", aiCoveTrigger: "" });
  const bubble = element({ action: "open-ai-cove", aiCoveBubble: "" });
  const popover = element({ aiCovePopover: "" });
  bubble.hidden = true;
  trigger.focused = false;
  trigger.focus = () => { trigger.focused = true; };
  const target = (action) => ({
    closest(selector) {
      if (selector === "[data-action]") return action;
      if (selector === "[data-ai-cove-popover]") return popover;
      return null;
    },
  });
  let onClick;
  let onKeydown;
  const document = {
    hidden: false,
    readyState: "complete",
    body: element(),
    addEventListener(type, handler) {
      if (type === "click") onClick = handler;
      if (type === "keydown") onKeydown = handler;
    },
    querySelector(selector) {
      if (selector === "[data-ai-cove-trigger]") return trigger;
      if (selector === "[data-ai-cove-bubble]") return bubble;
      return null;
    },
    querySelectorAll(selector) { return selector === "[data-action]" ? [trigger, bubble] : []; },
  };
  const opened = [];
  const window = {
    location: { href: "file:///turbo/src/index.html?tab=live" },
    history: { replaceState() {} },
    matchMedia: () => ({ matches: false }),
    addEventListener() {},
    setInterval() {},
    open(...args) { opened.push(args); },
  };

  await runApp(source, { document, window, URL, Intl, Error });
  onClick({ target: target(trigger) });
  assert.equal(bubble.hidden, false);
  assert.equal(trigger.attributes.get("aria-expanded"), "true");

  onKeydown({ key: "Escape" });
  assert.equal(bubble.hidden, true);
  assert.equal(trigger.attributes.get("aria-expanded"), "false");
  assert.equal(trigger.focused, true);

  onClick({ target: target(trigger) });
  onClick({ target: target(bubble) });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(opened, [["https://ai-cove.com", "_blank", "noopener,noreferrer"]]);
  assert.equal(bubble.hidden, true);
});

function textNode(value) {
  return {
    nodeType: 3,
    nodeName: "#text",
    nodeValue: value,
    parentNode: null,
    get nextSibling() {
      const siblings = this.parentNode?.childNodes ?? [];
      return siblings[siblings.indexOf(this) + 1] ?? null;
    },
    cloneNode() { return textNode(this.nodeValue); },
    remove() {
      const siblings = this.parentNode?.childNodes;
      if (!siblings) return;
      siblings.splice(siblings.indexOf(this), 1);
      this.parentNode = null;
    },
  };
}

function domNode(name, attributes = {}, children = []) {
  let classNames = new Set(String(attributes.class ?? "").split(/\s+/u).filter(Boolean));
  const listeners = new Map();
  const node = {
    nodeType: 1,
    nodeName: name.toUpperCase(),
    tagName: name.toUpperCase(),
    parentNode: null,
    childNodes: [],
    open: false,
    inert: false,
    _attributes: new Map(Object.entries(attributes)),
    classList: {
      add(...names) {
        names.forEach((className) => classNames.add(className));
        node._attributes.set("class", Array.from(classNames).join(" "));
      },
      remove(...names) {
        names.forEach((className) => classNames.delete(className));
        if (classNames.size) node._attributes.set("class", Array.from(classNames).join(" "));
        else node._attributes.delete("class");
      },
      contains(className) { return classNames.has(className); },
    },
    get attributes() {
      return Array.from(this._attributes, ([attributeName, value]) => ({ name: attributeName, value }));
    },
    get firstChild() { return this.childNodes[0] ?? null; },
    get nextSibling() {
      const siblings = this.parentNode?.childNodes ?? [];
      return siblings[siblings.indexOf(this) + 1] ?? null;
    },
    getAttribute(attributeName) { return this._attributes.get(attributeName) ?? null; },
    setAttribute(attributeName, value) {
      this._attributes.set(attributeName, String(value));
      if (attributeName === "class") classNames = new Set(String(value).split(/\s+/u).filter(Boolean));
    },
    removeAttribute(attributeName) {
      this._attributes.delete(attributeName);
      if (attributeName === "class") classNames.clear();
    },
    addEventListener(type, handler) {
      if (!listeners.has(type)) listeners.set(type, new Set());
      listeners.get(type).add(handler);
    },
    removeEventListener(type, handler) { listeners.get(type)?.delete(handler); },
    insertBefore(child, reference) {
      child.remove?.();
      const index = reference ? this.childNodes.indexOf(reference) : -1;
      this.childNodes.splice(index < 0 ? this.childNodes.length : index, 0, child);
      child.parentNode = this;
      return child;
    },
    cloneNode(deep = false) {
      return domNode(name, Object.fromEntries(this._attributes), deep ? this.childNodes.map((child) => child.cloneNode(true)) : []);
    },
    remove() {
      const siblings = this.parentNode?.childNodes;
      if (!siblings) return;
      siblings.splice(siblings.indexOf(this), 1);
      this.parentNode = null;
    },
  };
  children.forEach((child) => node.insertBefore(child, null));
  return node;
}

async function liveTailHarness() {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  let status = {
    serviceHealthy: true,
    configState: "managed",
    recentRequests: [
      { id: 1, timestampMs: 1_000, status: 200, path: "/v1/responses", rawBytes: 100, sentBytes: 60, transport: "HTTP", result: "success" },
    ],
    trafficWindows: [],
  };
  const requestStream = element();
  requestStream.innerHTML = "";
  requestStream.insertAdjacentHTML = (_position, value) => { requestStream.innerHTML += value; };
  let onScroll;
  const terminal = {
    scrollHeight: 300,
    scrollTop: 0,
    clientHeight: 100,
    addEventListener(type, handler) {
      if (type === "scroll") onScroll = handler;
    },
  };
  const follow = element({ action: "follow-live", liveFollow: "" });
  follow.hidden = true;
  follow.closest = (selector) => selector === "[data-action]" ? follow : null;
  const followLabel = element({ liveFollowLabel: "" });
  const selectors = new Map([
    ["[data-request-stream]", requestStream],
    [".c-terminal__window", terminal],
    ["[data-live-follow]", follow],
    ["[data-live-follow-label]", followLabel],
  ]);
  let onClick;
  let onTick;
  const document = {
    hidden: false,
    readyState: "complete",
    body: element(),
    addEventListener(type, handler) {
      if (type === "click") onClick = handler;
    },
    querySelector(selector) { return selectors.get(selector) ?? null; },
    querySelectorAll() { return []; },
  };
  const window = {
    __TAURI__: { core: { invoke: async () => status } },
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval(handler) { onTick = handler; },
  };

  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));

  return {
    follow,
    followLabel,
    requestStream,
    terminal,
    async click(action) {
      const control = action === "follow-live" ? follow : element({ action });
      control.closest = (selector) => selector === "[data-action]" ? control : null;
      onClick({ target: control });
      await new Promise((resolve) => setImmediate(resolve));
    },
    scrollTo(scrollTop) {
      terminal.scrollTop = scrollTop;
      onScroll();
    },
    setRequests(recentRequests) { status = { ...status, recentRequests }; },
    tick: () => onTick(),
  };
}

async function liveRecoveryHarness(status) {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const recovery = motionElement({}, true);
  const title = element();
  const message = element();
  const action = element({ action: "open-config" });
  const selectors = new Map([
    ["[data-live-recovery]", recovery],
    ["[data-live-recovery-title]", title],
    ["[data-live-recovery-message]", message],
    ["[data-live-recovery-action]", action],
  ]);
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelector(selector) { return selectors.get(selector) ?? null; },
    querySelectorAll() { return []; },
  };
  const window = {
    __TAURI__: { core: { invoke: async () => status } },
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval() {},
  };

  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));
  return { action, message, recovery, title };
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
  assert.equal(states[1].textContent, "等待首次连接验证");
  assert.equal(states[2].textContent, "0");
  assert.deepEqual(invoked, ["get_app_status"]);
});

test("仅观察到 WebSocket 握手时保持后端的等待请求状态", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const codex = element({ state: "restart-runtime", status: "waiting" });
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelector() { return null; },
    querySelectorAll(selector) { return selector === "[data-state]" ? [codex] : []; },
  };
  const window = {
    __TAURI__: {
      core: {
        invoke: async () => ({
          serviceHealthy: true,
          configState: "managed",
          codexState: "waiting_request",
          restartRequired: false,
          websocketHandshakes: 1,
        }),
      },
    },
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval() {},
  };

  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(codex.textContent, "等待请求");
  assert.equal(codex.dataset.status, "waiting");
});

test("Codex 生效状态完全由后端驱动", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const codex = element({ state: "restart-runtime", status: "waiting" });
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelector() { return null; },
    querySelectorAll(selector) { return selector === "[data-state]" ? [codex] : []; },
  };
  const window = {
    __TAURI__: {
      core: {
        invoke: async () => ({
          serviceHealthy: true,
          configState: "managed",
          codexState: "active",
          requests: 0,
          websocketHandshakes: 0,
        }),
      },
    },
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval() {},
  };

  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(codex.textContent, "已生效");
  assert.equal(codex.dataset.status, "verified");
});

test("Codex 待重启时只显示行内动作和 hover 提示", async () => {
  // Given: 后端明确返回需要重启，实时状态行里有一个重启动作。
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const restart = element({ action: "restart-codex", required: "false", restartHint: "" });
  const recovery = motionElement({}, true);
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelector(selector) {
      if (selector === "[data-live-recovery]") return recovery;
      return null;
    },
    querySelectorAll(selector) {
      if (selector === "[data-action]") return [restart];
      return [];
    },
  };
  const window = {
    __TAURI__: { core: { invoke: async () => ({ serviceHealthy: true, configState: "managed", codexState: "restart_required", restartRequired: true }) } },
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval() {},
  };

  // When: 首次真实状态完成渲染。
  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));

  // Then: CSS 显示手动重启按钮，说明放在 hover 提示中而非底部恢复卡片。
  assert.equal(restart.dataset.required, "true");
  assert.equal(restart.attributes.get("title"), "配置已写入，重启后会重新验证传输通道。");
  assert.match(restart.attributes.get("aria-label"), /重启 Codex/);
  assert.equal(recovery.hidden, true);
});

test("WebSocket 已恢复但 Responses 持续直连 HTTP 时提示旧任务重启 Codex", async () => {
  // Given: 最近有成功 Hybrid WS，随后五次直连 HTTP 占八成以上并持续超过三十秒。
  const now = Date.now();
  const recentRequests = [
    { id: 1, timestampMs: now - 55_000, path: "/v1/responses", route: "hybridWs", result: "success" },
    ...[50, 40, 30, 20, 10].map((ageSeconds, index) => ({
      id: index + 2,
      timestampMs: now - ageSeconds * 1_000,
      path: "/v1/responses",
      route: "directHttp",
    })),
  ];

  // When: Turbo 渲染已验证且健康的运行状态。
  const { action, message, recovery, title } = await liveRecoveryHarness({
    serviceHealthy: true,
    configState: "managed",
    codexState: "active",
    websocketEnabled: true,
    websocketVerified: true,
    websocketState: "connected",
    recentRequests,
  });

  // Then: 明确说明 Turbo WebSocket 已恢复，仅旧任务保留 HTTP，并提供现有重启操作。
  assert.equal(recovery.hidden, false);
  assert.equal(title.textContent, "部分旧任务仍在使用 HTTP");
  assert.equal(message.textContent, "Turbo 的 WebSocket 已恢复，但部分旧任务仍停留在 HTTP。建议完成当前操作后重启 Codex。");
  assert.equal(action.dataset.action, "restart-codex");
});

test("没有近期 WebSocket 成功证据时保留通用 HTTP 降级提示", async () => {
  // Given: 最近只有持续超过三十秒的 Responses 直连 HTTP，没有成功 Hybrid WS。
  const now = Date.now();
  const recentRequests = [60, 50, 40, 30, 20, 10].map((ageSeconds, index) => ({
    id: index + 1,
    timestampMs: now - ageSeconds * 1_000,
    path: "/v1/responses",
    route: "directHttp",
    result: "success",
  }));

  // When: Turbo 渲染已验证且健康的运行状态。
  const { action, message, recovery, title } = await liveRecoveryHarness({
    serviceHealthy: true,
    configState: "managed",
    codexState: "active",
    websocketEnabled: true,
    websocketState: "connected",
    recentRequests,
  });

  // Then: 保留通用提示，不声称 WebSocket 已经恢复。
  assert.equal(recovery.hidden, false);
  assert.equal(title.textContent, "Codex 可能仍在使用 HTTP");
  assert.match(message.textContent, /部分任务近期持续未建立 WebSocket/);
  assert.equal(action.dataset.action, "restart-codex");
});

test("最新 Responses 请求已恢复 WebSocket 时不再提示重启", async () => {
  // Given: 一段持续 HTTP 降级之后，最新请求已经通过 Hybrid WS 完成。
  const now = Date.now();
  const recentRequests = [50, 40, 30, 20, 10].map((ageSeconds, index) => ({
    id: index + 1,
    timestampMs: now - ageSeconds * 1_000,
    path: "/v1/responses",
    route: "directHttp",
  }));
  recentRequests.push({ id: 6, timestampMs: now - 5_000, path: "/v1/responses", route: "hybridWs" });

  // When: Turbo 渲染当前状态。
  const { recovery } = await liveRecoveryHarness({
    serviceHealthy: true,
    configState: "managed",
    codexState: "active",
    websocketEnabled: true,
    websocketState: "connected",
    recentRequests,
  });

  // Then: 已恢复的通道不再提示用户重复重启。
  assert.equal(recovery.hidden, true);
});

test("少量直连 HTTP 不提示重启 Codex", async () => {
  // Given: 配置正常，但最近只有四次 Responses 直连 HTTP。
  const now = Date.now();
  const recentRequests = [240, 180, 120, 60].map((ageSeconds, index) => ({
    id: index + 1,
    timestampMs: now - ageSeconds * 1_000,
    path: "/v1/responses",
    route: "directHttp",
  }));

  // When: Turbo 渲染当前状态。
  const { recovery } = await liveRecoveryHarness({
    serviceHealthy: true,
    configState: "managed",
    codexState: "active",
    websocketEnabled: true,
    websocketState: "connected",
    recentRequests,
  });

  // Then: 冷启动或零散 HTTP 不产生恢复提示。
  assert.equal(recovery.hidden, true);
});

test("WebSocket 已关闭时不把直连 HTTP 误判为降级", async () => {
  // Given: 用户主动关闭 WebSocket，最近请求自然全部使用 HTTP。
  const now = Date.now();
  const recentRequests = [60, 50, 40, 30, 20, 10].map((ageSeconds, index) => ({
    id: index + 1,
    timestampMs: now - ageSeconds * 1_000,
    path: "/v1/responses",
    route: "directHttp",
  }));

  // When: Turbo 渲染当前状态。
  const { recovery } = await liveRecoveryHarness({
    serviceHealthy: true,
    configState: "managed",
    codexState: "active",
    websocketEnabled: false,
    recentRequests,
  });

  // Then: 不提示重启 Codex。
  assert.equal(recovery.hidden, true);
});

test("状态读取失败时提供就地恢复和折叠技术详情", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const recovery = element();
  const title = element();
  const message = element();
  const action = element({ action: "open-config" });
  const details = element();
  const detail = element();
  const selectors = new Map([
    ["[data-live-recovery]", recovery],
    ["[data-live-recovery-title]", title],
    ["[data-live-recovery-message]", message],
    ["[data-live-recovery-action]", action],
    ["[data-live-recovery-details]", details],
    ["[data-live-recovery-detail]", detail],
  ]);
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelector(selector) { return selectors.get(selector) ?? null; },
    querySelectorAll() { return []; },
  };
  const window = {
    __TAURI__: { core: { invoke: async () => { throw new Error("socket closed"); } } },
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval() {},
  };

  await runApp(source, { document, Error, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(recovery.hidden, false);
  assert.equal(title.textContent, "本地服务离线");
  assert.match(message.textContent, /检查服务与上游/);
  assert.equal(action.dataset.action, "open-config");
  assert.equal(details.hidden, false);
  assert.equal(detail.textContent, "socket closed");
});

test("Strands 数量跟随五项聚合状态转绿", async () => {
  // Given: 五项状态全部通过，随后 WebSocket zstd 失去验证。
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const strands = [element({ strands: "", count: "0" }), element({ strands: "", count: "0" })];
  const counts = [];
  let status = {
    serviceHealthy: true,
    configState: "managed",
    codexState: "active",
    desktopRestarted: true,
    restartRequired: false,
    compressionEnabled: true,
    compressionVerified: true,
    websocketEnabled: true,
    websocketVerified: true,
    websocketZstdVerified: true,
  };
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelector(selector) {
      return selector === "[data-strands]" ? strands[0] : null;
    },
    querySelectorAll(selector) { return selector === "[data-strands]" ? strands : []; },
  };
  let onTick;
  const window = {
    __TAURI__: { core: { invoke: async () => status } },
    TurboStrands: { setCount(count) { counts.push(count); } },
    location: { href: "tauri://localhost/?tab=config" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval(handler) { onTick = handler; },
  };

  // When: 首次状态渲染完成。
  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));

  // Then: 五项全绿时显示五条。
  assert.equal(counts.at(-1), 5);
  assert.deepEqual(strands.map((canvas) => canvas.dataset.count), ["5", "5"]);

  // When: 新一轮状态使 WebSocket 聚合项不再全绿。
  status = { ...status, websocketZstdVerified: false };
  await onTick();

  // Then: 只保留其余四条。
  assert.equal(counts.at(-1), 4);
  assert.deepEqual(strands.map((canvas) => canvas.dataset.count), ["4", "4"]);
});

test("连接摘要持续刷新且两个入口共享面板状态", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const trigger = element({ action: "toggle-connections" });
  trigger.focused = false;
  trigger.focus = () => { trigger.focused = true; };
  trigger.closest = (selector) => selector === "[data-action]" ? trigger : null;
  const panel = motionElement({ connectionPanel: "" }, true);
  const panelClasses = new Set();
  panel.classList = {
    add(name) { panelClasses.add(name); },
    remove(name) { panelClasses.delete(name); },
  };
  panel.getBoundingClientRect = () => ({ height: 400 });
  const summaryTrigger = motionElement({ action: "toggle-connections", connectionSummaryTrigger: "" });
  summaryTrigger.getBoundingClientRect = () => {
    const translated = Number(String(summaryTrigger.style.transform || "").match(/translate3d\(0, (-?[\d.]+)px,/u)?.[1] || 0);
    const panelInFlow = !panel.hidden
      && !panelClasses.has("is-closing")
      && !panelClasses.has("is-drag-preview");
    return { top: (panelInFlow ? 458 : 58) + translated };
  };
  summaryTrigger.focused = false;
  summaryTrigger.focus = () => { summaryTrigger.focused = true; };
  summaryTrigger.closest = (selector) => selector === "[data-action]" ? summaryTrigger : null;
  const dock = element({ connectionDock: "" });
  dock.offsetWidth = 456;
  let dockOffset = "0px";
  dock.style.setProperty = (name, value) => {
    if (name === "--connection-dock-offset") dockOffset = value;
  };
  dock.classList = { add() {}, remove() {} };
  const grip = summaryTrigger;
  const gripHandlers = new Map();
  grip.addEventListener = (type, handler) => gripHandlers.set(type, handler);
  grip.setPointerCapture = () => {};
  grip.releasePointerCapture = () => {};
  const total = element({ connectionTotal: "" });
  const summaryUp = element({ connectionSummary: "up" });
  const summaryDown = element({ connectionSummary: "down" });
  const summaryIdle = element({ connectionSummary: "idle" });
  const prewarmCount = element({ connectionCount: "prewarm" });
  const boundCount = element({ connectionCount: "bound" });
  const transitionCount = element({ connectionCount: "transitions" });
  const closedCount = element({ connectionCount: "closed" });
  const prewarm = element({ connectionList: "prewarm" });
  const bound = element({ connectionList: "bound" });
  const transitions = element({ connectionList: "transitions" });
  let transitionHtml = "";
  let transitionDetails = [];
  Object.defineProperty(transitions, "innerHTML", {
    get() { return transitionHtml; },
    set(value) {
      transitionHtml = String(value);
      transitionDetails = Array.from(
        transitionHtml.matchAll(/data-transition-id="([^"]+)"/gu),
        (match) => ({ dataset: { transitionId: match[1] }, open: false }),
      );
    },
  });
  transitions.querySelectorAll = (selector) => selector === "details[open][data-transition-id]"
    ? transitionDetails.filter((item) => item.open)
    : transitionDetails;
  const closed = element({ connectionList: "closed" });
  const closedDensitySession = element({ autoDensity: "", threadId: "thread-density" });
  closedDensitySession.dataset.density = "full";
  const measuredPart = (kind, fullWidth, compactWidth = fullWidth) => ({
    kind,
    get scrollWidth() {
      return closedDensitySession.dataset.density === "compact" ? compactWidth : fullWidth;
    },
    getBoundingClientRect() {
      return { width: Math.min(8, this.scrollWidth) };
    },
  });
  const measuredChip = () => ({
    kind: "chip",
    children: [
      measuredPart("icon", 10),
      measuredPart("name", 50, 14),
      measuredPart("glyph", 12),
      measuredPart("hover", 180),
    ],
  });
  const closedConnections = {
    kind: "connections",
    clientWidth: 80,
    children: [measuredChip(), measuredChip()],
  };
  closedDensitySession.querySelector = (selector) => selector === ".c-connection-session__connections"
    ? closedConnections
    : null;
  closed.querySelectorAll = (selector) => selector === "[data-auto-density]"
    ? [closedDensitySession]
    : [];
  const message = element({ connectionMessage: "" });
  message.hidden = true;
  const selectors = new Map([
    ["[data-connection-panel]", panel],
    ["[data-connection-dock]", dock],
    ["[data-connection-grip]", grip],
    ["[data-connection-total]", total],
    ['[data-connection-summary="up"]', summaryUp],
    ['[data-connection-summary="down"]', summaryDown],
    ['[data-connection-summary="idle"]', summaryIdle],
    ['[data-connection-count="prewarm"]', prewarmCount],
    ['[data-connection-count="bound"]', boundCount],
    ['[data-connection-count="transitions"]', transitionCount],
    ['[data-connection-count="closed"]', closedCount],
    ['[data-connection-list="prewarm"]', prewarm],
    ['[data-connection-list="bound"]', bound],
    ['[data-connection-list="transitions"]', transitions],
    ['[data-connection-list="closed"]', closed],
    ["[data-connection-message]", message],
    ["[data-connection-summary-trigger]", summaryTrigger],
    ['[data-action="toggle-connections"]', summaryTrigger],
  ]);
  const invoked = [];
  const infoCalls = [];
  const threadInfos = {
    "thread-12345678-alpha": { name: "Nash", parentName: "Turbo 主会话", isSubagent: true },
    "thread-12345678-beta": { name: "", parentName: null, isSubagent: false },
    "thread-released": { name: "已结束会话", parentName: null, isSubagent: false },
  };
  const snapshot = {
    currentConnections: 10,
    prewarm: 2,
    boundThreads: [
      { id: "S001", threadId: "thread-12345678-alpha", activity: "down", idleSeconds: 0, reclaimPolicy: "threadEnd" },
      { id: "S002", threadId: "thread-12345678-alpha", activity: "idle", idleSeconds: 18, reclaimPolicy: "threadEnd" },
      { id: "S004", threadId: "thread-12345678-beta", activity: "up", idleSeconds: 0, reclaimPolicy: "threadEnd" },
      { id: "S010", threadId: "thread-12345678-gamma", activity: "down", idleSeconds: 0, reclaimPolicy: "threadEnd" },
      { id: "S011", threadId: "thread-12345678-gamma", activity: "idle", idleSeconds: 4, reclaimPolicy: "threadEnd" },
      { id: "S012", threadId: "thread-12345678-gamma", activity: "up", idleSeconds: 0, reclaimPolicy: "threadEnd" },
    ],
    transitions: [
      { id: "T003", threadId: "thread-12345678-alpha", connectionId: "S005", label: "恢复绑定连接", stage: "等待可用连接", detail: "上游关闭", elapsedSeconds: 2 },
      { id: "T007", threadId: "thread-12345678-alpha", connectionId: "S007", label: "建立绑定连接", stage: "正在握手", detail: "等待上游确认", elapsedSeconds: 1 },
    ],
    recentClosed: [
      { id: "C001", threadId: "thread-12345678-alpha", connectionId: "S003", reason: "上游连接关闭", agoSeconds: 8, normal: false },
      { id: "C004", threadId: "thread-12345678-alpha", connectionId: "S001", reason: "连接空闲关闭", agoSeconds: 6, normal: true },
      { id: "C002", threadId: "thread-12345678-alpha", connectionId: "S006", reason: "连接恢复失败", agoSeconds: 4, normal: false },
      { id: "C003", threadId: "thread-released", connectionId: "S009", reason: "Codex 线程结束", agoSeconds: 3, normal: true },
    ],
  };
  let onClick;
  let onKeydown;
  let onTick;
  const document = {
    hidden: false,
    readyState: "complete",
    body: element(),
    addEventListener(type, handler) {
      if (type === "click") onClick = handler;
      if (type === "keydown") onKeydown = handler;
    },
    querySelector(selector) { return selectors.get(selector) ?? null; },
    querySelectorAll(selector) {
      if (selector === '[data-action="toggle-connections"]') return [summaryTrigger, trigger];
      if (selector === "[data-action]") return [trigger];
      return [];
    },
  };
  const window = {
    __TAURI__: { core: { invoke: async (command, args) => {
      if (command === "get_codex_thread_info") {
        infoCalls.push(args.threadId);
        return threadInfos[args.threadId] ?? null;
      }
      invoked.push(command);
      return command === "get_connection_snapshot"
        ? snapshot
        : { serviceHealthy: true, configState: "managed", trafficWindows: [], recentRequests: [] };
    } } },
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    matchMedia: () => ({ matches: false }),
    innerWidth: 1_000,
    addEventListener() {},
    setInterval(handler) { onTick = handler; },
  };
  let styleReads = 0;
  const getComputedStyle = (target) => {
    styleReads += 1;
    if (target.kind === "connections") return { columnGap: "5px" };
    if (target.kind === "chip") return { columnGap: "5px", paddingLeft: "4px", paddingRight: "4px" };
    if (target.kind === "hover") return { display: "grid", position: "absolute" };
    if (target.kind === "name" && closedDensitySession.dataset.density === "state-only") {
      return { display: "none", position: "static" };
    }
    return { display: "block", position: "static" };
  };

  await runApp(source, { document, Error, window, URL, Intl, getComputedStyle });
  await new Promise((resolve) => setImmediate(resolve));
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(invoked, ["get_app_status", "get_connection_snapshot"]);
  assert.equal(summaryUp.textContent, "2");
  assert.equal(summaryDown.textContent, "2");
  assert.equal(summaryIdle.textContent, "2");
  assert.equal(summaryTrigger.dataset.tooltip, "左右拖动移动 · 点击或下拉展开");
  assert.match(summaryTrigger.attributes.get("aria-label"), /点击或下拉展开连接检查器/);

  gripHandlers.get("pointerdown")({ button: 0, pointerId: 0, clientX: 100, clientY: 100, preventDefault() {} });
  gripHandlers.get("pointermove")({ pointerId: 0, clientX: 140, clientY: 100, preventDefault() {} });
  gripHandlers.get("pointerup")({ pointerId: 0, clientX: 140, clientY: 100 });
  assert.equal(dockOffset, "40px");
  assert.equal(panel.hidden, true);
  gripHandlers.get("click")({ preventDefault() {}, stopPropagation() {} });
  gripHandlers.get("keydown")({ key: "Home", preventDefault() {} });
  assert.equal(dockOffset, "0px");

  onClick({ target: summaryTrigger });
  await new Promise((resolve) => setImmediate(resolve));
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(panel.hidden, false);
  assert.equal(panel.attributes.get("aria-hidden"), "false");
  assert.equal(dock.dataset.open, "true");
  assert.equal(dock.dataset.phase, "opening");
  assert.deepEqual(
    Array.from(summaryTrigger.animations.at(-1).keyframes, (frame) => frame.transform),
    ["translate3d(0, -400px, 0)", "translate3d(0, -72px, 0)", "translate3d(0, 0, 0)"],
  );
  assert.deepEqual(
    Array.from(panel.animations.at(-1).keyframes, (frame) => frame.clipPath),
    ["inset(0 0 100% 0 round 14px)", "inset(0 0 18% 0 round 14px)", "inset(0 0 0 0 round 14px)"],
  );
  panel.animations.at(-1).onfinish();
  assert.equal(dock.dataset.phase, "open");
  assert.equal(trigger.attributes.get("aria-expanded"), "true");
  assert.equal(summaryTrigger.attributes.get("aria-expanded"), "true");
  assert.equal(total.textContent, "10");
  assert.match(prewarm.innerHTML, /<strong>P01<\/strong>[\s\S]*?<dt>状态<\/dt><dd>空白预热<\/dd>[\s\S]*?<dt>回收<\/dt><dd>容量压力时回收<\/dd>/);
  assert.match(bound.innerHTML, /data-thread-id="thread-12345678-alpha"/);
  assert.match(bound.innerHTML, /data-density="compact"[^>]*data-thread-id="thread-12345678-alpha"/);
  assert.match(bound.innerHTML, /data-density="full"[^>]*data-thread-id="thread-12345678-beta"/);
  assert.match(bound.innerHTML, /data-density="state-only"[^>]*data-thread-id="thread-12345678-gamma"/);
  assert.match(bound.innerHTML, /data-short-name="01"[^>]*>连接 01<\/strong>/);
  assert.match(bound.innerHTML, /data-thread-id="thread-12345678-alpha"[^>]*>[\s\S]*?会话 01[\s\S]*?×2[\s\S]*?c-connection-session__separator[\s\S]*?连接 01[\s\S]*?连接 02/);
  assert.match(bound.innerHTML, /data-thread-id="thread-12345678-beta"[^>]*>[\s\S]*?会话 02/);
  assert.match(bound.innerHTML, /c-connection-session__summary"[^>]*aria-label="会话 01，子会话，2 条连接，发送 0，接收 1，空闲 1"[^>]*>\s*<svg class="c-session-icon"/);
  assert.match(bound.innerHTML, /<span class="c-hover-card" aria-hidden="true"><strong>会话 01<\/strong><dl><div><dt>会话名称<\/dt><dd>Nash<\/dd><\/div><div><dt>会话类型<\/dt><dd>子会话<\/dd><\/div><div><dt>所属父会话<\/dt><dd>Turbo 主会话<\/dd>/);
  const boundSessionHover = bound.innerHTML.match(/<span class="c-hover-card"[^>]*><strong>会话 01<\/strong>[\s\S]*?<\/span>/)?.[0] ?? "";
  assert.doesNotMatch(boundSessionHover, /会话 ID|线程 ID|thread-12345678-alpha/);
  assert.match(bound.innerHTML, /data-thread-id="thread-12345678-alpha"[^>]*>[\s\S]*?<svg class="c-session-icon" data-connection-state="active" data-session-kind="subagent"[\s\S]*?c-session-icon__branch/);
  assert.match(bound.innerHTML, /<svg class="c-session-icon"[^>]*><path d="M3 1\.75h8[^>]*\/><\/svg>/);
  assert.doesNotMatch(bound.innerHTML, /<svg class="c-session-icon"[^>]*>[\s\S]*?<rect/);
  const betaStart = bound.innerHTML.indexOf('data-thread-id="thread-12345678-beta"');
  const gammaStart = bound.innerHTML.indexOf('data-thread-id="thread-12345678-gamma"');
  assert.match(bound.innerHTML.slice(betaStart, gammaStart), /<dt>会话名称<\/dt><dd>-<\/dd>/);
  assert.match(bound.innerHTML.slice(gammaStart), /<dt>会话名称<\/dt><dd>-<\/dd>/);
  assert.match(bound.innerHTML, /data-connection-id="S001"[\s\S]*?连接 01/);
  assert.match(bound.innerHTML, /data-connection-id="S002"[\s\S]*?连接 02/);
  assert.match(bound.innerHTML, /c-connection-chip[^>]*data-connection-id="S001"[\s\S]*?<i class="c-ws-icon"/);
  const boundConnectionHover = bound.innerHTML.match(/data-connection-id="S001"[\s\S]*?<span class="c-hover-card"[^>]*>[\s\S]*?<\/span>/)?.[0] ?? "";
  assert.match(boundConnectionHover, /<dt>连接 ID<\/dt><dd>S001<\/dd>/);
  assert.doesNotMatch(boundConnectionHover, /线程 ID|会话 ID|thread-12345678-alpha/);
  assert.doesNotMatch(bound.innerHTML, /c-session-pin|c-connection-session__metric|data-action="pin-session"/);
  assert.doesNotMatch(bound.innerHTML, /线程 12345678/);
  assert.match(bound.innerHTML, /<svg class="c-connection-idle" viewBox="0 0 18 14" data-direction="up-right"[^>]*>(?:<path[^>]* \/>){3}<\/svg>/);
  assert.doesNotMatch(bound.innerHTML, /zzz/);
  assert.match(bound.innerHTML, /随线程结束回收/);
  assert.equal((transitions.innerHTML.match(/<details/g) ?? []).length, 2);
  assert.match(transitions.innerHTML, /data-transition-id="T003"[\s\S]*?会话 01 · 连接 03/);
  assert.match(transitions.innerHTML, /data-transition-id="T007"[\s\S]*?会话 01 · 连接 04/);
  assert.doesNotMatch(transitions.innerHTML, /transition-session|×2/);
  assert.match(closed.innerHTML, /data-thread-id="thread-12345678-alpha"[^>]*>[\s\S]*?c-session-icon" data-connection-state="active"[\s\S]*?<dt>会话状态<\/dt><dd>仍在绑定<\/dd>/);
  assert.match(closed.innerHTML, /data-thread-id="thread-12345678-alpha"[^>]*>[\s\S]*?会话 01[\s\S]*?×3[\s\S]*?c-connection-session__separator[\s\S]*?data-connection-event-id="C004"[\s\S]*?data-connection-event-id="C001"[\s\S]*?data-connection-event-id="C002"/);
  assert.match(closed.innerHTML, /data-thread-id="thread-released"[^>]*>[\s\S]*?c-session-icon" data-connection-state="closed"[\s\S]*?<dt>会话状态<\/dt><dd>已释放<\/dd>/);
  const closedSessionHover = closed.innerHTML.match(/<span class="c-hover-card"[^>]*><strong>会话 01<\/strong>[\s\S]*?<\/span>/)?.[0] ?? "";
  assert.match(closedSessionHover, /会话名称|会话类型|所属父会话|Turbo 主会话/);
  assert.doesNotMatch(closedSessionHover, /会话 ID|线程 ID|thread-12345678-alpha/);
  assert.match(closed.innerHTML, /data-connection-id="S003"[\s\S]*?连接 05/);
  assert.match(closed.innerHTML, /data-connection-id="S006"[\s\S]*?连接 06/);
  assert.match(closed.innerHTML, /data-connection-event-id="C001"/);
  assert.match(closed.innerHTML, /data-connection-event-id="C002"/);
  assert.match(closed.innerHTML, /data-connection-event-id="C003"/);
  assert.match(closed.innerHTML, /上游连接关闭/);
  assert.match(closed.innerHTML, /连接恢复失败/);
  assert.match(closed.innerHTML, /data-auto-density[^>]*data-density="full"/);
  assert.equal(closedDensitySession.dataset.density, "state-only");

  const readsAfterFirstLayout = styleReads;
  await onTick();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(styleReads, readsAfterFirstLayout);

  closedConnections.clientWidth = 120;
  await onTick();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(closedDensitySession.dataset.density, "compact");

  closedConnections.clientWidth = 200;
  await onTick();
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(closedDensitySession.dataset.density, "full");

  gripHandlers.get("pointerdown")({ button: 0, pointerId: 1, clientX: 100, clientY: 100, preventDefault() {} });
  gripHandlers.get("pointermove")({ pointerId: 1, clientX: 170, clientY: 100, preventDefault() {} });
  gripHandlers.get("pointerup")({ pointerId: 1, clientX: 170, clientY: 100 });
  assert.equal(dockOffset, "70px");
  gripHandlers.get("click")({ preventDefault() {} });
  assert.equal(panel.hidden, false);

  gripHandlers.get("pointerdown")({ button: 0, pointerId: 2, clientX: 170, clientY: 100, preventDefault() {} });
  gripHandlers.get("pointermove")({ pointerId: 2, clientX: 170, clientY: 64, preventDefault() {} });
  assert.equal(dock.dataset.phase, "dragging-close");
  assert.equal(panelClasses.has("is-drag-preview"), true);
  assert.equal(summaryTrigger.style.transform, "translate3d(0, 364px, 0)");
  assert.equal(panel.style.clipPath, "inset(0 0 9% 0 round 14px)");
  gripHandlers.get("pointerup")({ pointerId: 2, clientX: 170, clientY: 64 });
  assert.equal(panel.hidden, false);
  assert.equal(panel.attributes.get("aria-hidden"), "true");
  assert.equal(dock.dataset.open, "false");
  assert.equal(dock.dataset.phase, "closing");
  assert.equal(panelClasses.has("is-closing"), true);
  assert.deepEqual(
    Array.from(summaryTrigger.animations.at(-1).keyframes, (frame) => frame.transform),
    ["translate3d(0, 364px, 0)", "translate3d(0, 65.52px, 0)", "translate3d(0, 0, 0)"],
  );
  const closingClipPaths = Array.from(panel.animations.at(-1).keyframes, (frame) => frame.clipPath);
  assert.ok(Math.abs(Number(closingClipPaths[0].match(/inset\(0 0 ([\d.]+)%/u)?.[1]) - 9) < 0.001);
  assert.equal(closingClipPaths[1], "inset(0 0 100% 0 round 14px)");
  panel.animations.at(-1).onfinish();
  assert.equal(panel.hidden, true);
  assert.equal(dock.dataset.phase, "closed");
  assert.equal(panelClasses.has("is-closing"), false);
  gripHandlers.get("click")({ preventDefault() {} });

  gripHandlers.get("pointerdown")({ button: 0, pointerId: 3, clientX: 170, clientY: 100, preventDefault() {} });
  gripHandlers.get("pointermove")({ pointerId: 3, clientX: 170, clientY: 300, preventDefault() {} });
  assert.equal(panel.hidden, false);
  assert.equal(dock.dataset.phase, "dragging-open");
  assert.equal(panelClasses.has("is-drag-preview"), true);
  assert.equal(summaryTrigger.style.transform, "translate3d(0, 200px, 0)");
  assert.equal(panel.style.clipPath, "inset(0 0 50% 0 round 14px)");
  gripHandlers.get("pointerup")({ pointerId: 3, clientX: 170, clientY: 300 });
  assert.equal(dock.dataset.open, "true");
  assert.equal(dock.dataset.phase, "opening");
  assert.equal(panelClasses.has("is-drag-preview"), false);
  assert.deepEqual(
    Array.from(panel.animations.at(-1).keyframes, (frame) => frame.clipPath),
    ["inset(0 0 50% 0 round 14px)", "inset(0 0 0 0 round 14px)"],
  );
  panel.animations.at(-1).onfinish();
  assert.equal(dock.dataset.phase, "open");
  gripHandlers.get("click")({ preventDefault() {}, stopPropagation() {} });

  onClick({ target: summaryTrigger });
  assert.equal(panel.hidden, false);
  assert.deepEqual(
    Array.from(panel.animations.at(-1).keyframes, (frame) => frame.clipPath),
    ["inset(0 0 0 0 round 14px)", "inset(0 0 82% 0 round 14px)", "inset(0 0 100% 0 round 14px)"],
  );
  panel.animations.at(-1).onfinish();
  assert.equal(panel.hidden, true);

  onClick({ target: trigger });
  assert.equal(panel.hidden, false);
  assert.equal(summaryTrigger.attributes.get("aria-expanded"), "true");
  onClick({ target: trigger });
  assert.equal(panel.hidden, false);
  assert.equal(summaryTrigger.attributes.get("aria-expanded"), "false");
  panel.animations.at(-1).onfinish();
  assert.equal(panel.hidden, true);
  onClick({ target: trigger });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(panel.hidden, false);

  // Given/When: 同一会话当前只剩空闲绑定。
  snapshot.boundThreads[0].activity = "idle";
  await onTick();
  await new Promise((resolve) => setImmediate(resolve));

  // Then: 近期关闭的会话图标与绑定区域复用同一个蓝色状态。
  assert.match(bound.innerHTML, /data-thread-id="thread-12345678-alpha"[^>]*>[\s\S]*?c-session-icon" data-connection-state="bound"/);
  assert.match(closed.innerHTML, /data-thread-id="thread-12345678-alpha"[^>]*>[\s\S]*?c-session-icon" data-connection-state="bound"/);

  transitionDetails[0].open = true;
  const infoCallCount = infoCalls.length;
  await onTick();
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(invoked.slice(-2), ["get_app_status", "get_connection_snapshot"]);
  assert.equal(infoCalls.length, infoCallCount);
  assert.equal(transitionDetails[0].dataset.transitionId, "T003");
  assert.equal(transitionDetails[0].open, true);

  // Given: 会话仍存在，恢复中的连接取代已经关闭的低号连接。
  snapshot.boundThreads = [
    snapshot.boundThreads[1],
    { id: "S005", threadId: "thread-12345678-alpha", activity: "up", idleSeconds: 0, reclaimPolicy: "threadEnd" },
    snapshot.boundThreads[2],
  ];
  snapshot.transitions = [];
  snapshot.recentClosed = [];
  await onTick();
  await new Promise((resolve) => setImmediate(resolve));

  // Then: 同一物理连接从恢复区进入绑定区后沿用原编号。
  assert.match(bound.innerHTML, /data-connection-id="S005"[\s\S]*?连接 03/);

  // Given: 会话 01 完全离开三个区域，随后出现新线程。
  snapshot.boundThreads = [snapshot.boundThreads[2]];
  await onTick();
  await new Promise((resolve) => setImmediate(resolve));
  snapshot.boundThreads = [
    snapshot.boundThreads[0],
    { id: "S006", threadId: "thread-new", activity: "idle", idleSeconds: 1, reclaimPolicy: "threadEnd" },
  ];
  await onTick();
  await new Promise((resolve) => setImmediate(resolve));

  // Then: 原会话 02 保号，新线程复用最低空闲的会话 01。
  assert.match(bound.innerHTML, /data-thread-id="thread-12345678-beta"[^>]*>[\s\S]*?会话 02/);
  assert.match(bound.innerHTML, /data-thread-id="thread-new"[^>]*>[\s\S]*?会话 01/);

  // Given/When: 后端滚动升级期间返回不含 currentConnections 的旧快照。
  delete snapshot.currentConnections;
  snapshot.prewarm = 3;
  snapshot.boundThreads = snapshot.boundThreads.slice(0, 1);
  await onTick();
  await new Promise((resolve) => setImmediate(resolve));

  // Then: 前端继续以预热加绑定数量显示兼容总数。
  assert.equal(total.textContent, "4");

  onKeydown({ key: "Escape", target: { closest: () => null } });
  assert.equal(panel.hidden, false);
  assert.equal(trigger.attributes.get("aria-expanded"), "false");
  assert.equal(trigger.focused, true);
  panel.animations.at(-1).onfinish();
  assert.equal(panel.hidden, true);
});

test("绑定会话双列且近期关闭保持单列", async () => {
  const html = await readFile(new URL("../src/index.html", import.meta.url), "utf8");
  const css = await readFile(new URL("../src/styles.css", import.meta.url), "utf8");
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");

  assert.ok(html.indexOf("data-connection-panel") < html.indexOf("data-connection-summary-trigger"));
  assert.match(html, /data-connection-summary-trigger[^>]*data-connection-grip/);
  assert.doesNotMatch(html, /close-connections|c-connection-grip/);
  assert.match(css, /data-connection-group="bound"[^}]*c-connection-sessions[^}]*grid-template-columns:\s*repeat\(2,\s*minmax\(0,\s*1fr\)\)/s);
  assert.match(css, /data-connection-group="closed"[^}]*c-connection-sessions[^}]*grid-template-columns:\s*minmax\(0,\s*1fr\)/s);
  assert.match(css, /c-connection-session__connections\s*>\s*\.c-connection-chip[^}]*flex:\s*1 1 0/s);
  assert.match(css, /\.c-connection-inspector\s*{[^}]*max-height:\s*calc\(100dvh[^}]*overflow-y:\s*auto[^}]*scrollbar-width:\s*none/s);
  assert.match(css, /\.c-connection-inspector::\-webkit-scrollbar\s*{[^}]*display:\s*none/s);
  assert.match(html, /c-connection-inspector__header[^>]*>[\s\S]*?<h2[^>]*><span class="c-connection-inspector__live">LIVE<\/span><span>WebSocket 连接<\/span><em>当前 <strong data-connection-total>0<\/strong> 条<\/em><\/h2>/);
  assert.match(css, /\.c-connection-dock\[data-open="true"\] \.c-connection-summary,\s*\.c-connection-dock\[data-phase="dragging-open"\] \.c-connection-summary,\s*\.c-connection-dock\[data-phase="dragging-close"\] \.c-connection-summary\s*{[^}]*gap:\s*7px[^}]*min-height:\s*var\(--turbo-connection-summary-open-height\)[^}]*padding:\s*0 9px 6px/s);
  assert.match(css, /\.c-connection-summary::after\s*{[^}]*width:\s*28px[^}]*height:\s*2px[^}]*opacity:\s*0[^}]*content:\s*""/s);
  assert.match(css, /data-open="false"\]\[data-phase="closed"\] \.c-connection-summary:hover::before[^}]*opacity:\s*1[^}]*visibility:\s*visible/s);
  assert.match(css, /data-open="true"\] \.c-connection-summary:hover::after[^}]*background:\s*var\(--c-accent\)[^}]*opacity:\s*1/s);
  assert.match(css, /\.c-connection-inspector\.is-closing,\s*\.c-connection-inspector\.is-drag-preview\s*{[^}]*position:\s*absolute[^}]*pointer-events:\s*none/s);
  assert.match(source, /previousSummaryTop[\s\S]*nextSummaryTop[\s\S]*translate3d\(0, \$\{shift\}px, 0\)/);
  assert.match(source, /dock\.dataset\.phase\s*=\s*connectionPanelOpen\s*\?\s*"opening"[\s\S]*?"closing"[\s\S]*?"closed"/);
  assert.match(source, /Math\.hypot\(deltaX, deltaY\)\s*>\s*3[\s\S]*drag\.axis\s*=\s*Math\.abs\(deltaX\)\s*>\s*Math\.abs\(deltaY\)\s*\?\s*"x"\s*:\s*"y"/);
  assert.match(source, /applyVerticalProgress\(\(drag\.startedOpen\s*\?\s*1\s*:\s*0\)\s*\+\s*deltaY\s*\/\s*drag\.panelHeight\)/);
  assert.match(source, /distance\s*>\s*-CONNECTION_DRAG_THRESHOLD_PX[\s\S]*distance\s*>=\s*CONNECTION_DRAG_THRESHOLD_PX/);
  assert.doesNotMatch(css, /\.c-connection-inspector\.is-overflowing/);
  assert.doesNotMatch(source, /close-connections/);
});

test("连续连接快照保留稳定节点、焦点和展开状态", async () => {
  const source = await readFile(new URL("../src/connection-dom.js", import.meta.url), "utf8");
  const clearedTimers = [];
  const document = {
    createElement() { return null; },
  };
  const window = {
    setTimeout() { return 7; },
    clearTimeout(timer) { clearedTimers.push(timer); },
  };

  vm.runInNewContext(source, { document, window, Intl });
  const { reconcileChildren } = window.TurboConnectionDOM;

  const connection = domNode("span", { "data-connection-key": "connection:S001" }, [textNode("空闲 18 秒")]);
  const transition = domNode("details", { "data-connection-key": "transition:S003" }, [textNode("恢复中 2 秒")]);
  transition.open = true;
  const session = domNode("div", { "data-connection-key": "bound:thread-alpha" }, [connection, transition]);
  const list = domNode("div", {}, [session]);
  const focusedNode = connection;
  const next = domNode("div", {}, [
    domNode("div", { "data-connection-key": "bound:thread-alpha" }, [
      domNode("span", { "data-connection-key": "connection:S001" }, [textNode("空闲 19 秒")]),
      domNode("span", { "data-connection-key": "connection:S002" }, [textNode("正在接收")]),
      domNode("details", { "data-connection-key": "transition:S003" }, [textNode("恢复中 3 秒")]),
    ]),
  ]);

  reconcileChildren(list, next);

  assert.equal(list.firstChild, session);
  assert.equal(session.childNodes[0], focusedNode);
  assert.equal(focusedNode.firstChild.nodeValue, "空闲 19 秒");
  assert.equal(session.childNodes[2], transition);
  assert.equal(transition.open, true);
  assert.equal(transition.firstChild.nodeValue, "恢复中 3 秒");

  reconcileChildren(list, domNode("div"));
  assert.equal(list.firstChild, session);
  assert.equal(session.classList.contains("is-leaving"), true);
  assert.equal(session.inert, true);

  reconcileChildren(list, next);
  assert.equal(list.firstChild, session);
  assert.equal(session.classList.contains("is-leaving"), false);
  assert.equal(session.inert, false);
  assert.equal(session.getAttribute("aria-hidden"), null);
  assert.deepEqual(clearedTimers, [7]);
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

test("页面切换按方向进入、实时页固定连接摘要不参与变换且保留面板状态", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const tabs = [element({ tab: "live" }), element({ tab: "statistics" }), element({ tab: "config" })];
  const liveSurface = motionElement();
  const panels = [
    motionElement({ panel: "live" }),
    motionElement({ panel: "statistics" }, true),
    motionElement({ panel: "config" }, true),
  ];
  panels[0].querySelector = (selector) => selector === ".c-console" ? liveSurface : null;
  const connectionSummary = motionElement({ action: "toggle-connections", connectionSummaryTrigger: "" });
  connectionSummary.closest = (selector) => selector === "[data-action]" ? connectionSummary : null;
  const connectionTrigger = element({ action: "toggle-connections" });
  const connectionPanel = motionElement({ connectionPanel: "" }, true);
  const connectionDock = element({ connectionDock: "" });
  const connectionSelectors = new Map([
    ["[data-connection-summary-trigger]", connectionSummary],
    ["[data-connection-panel]", connectionPanel],
    ["[data-connection-dock]", connectionDock],
  ]);
  let onClick;
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener(type, handler) { if (type === "click") onClick = handler; },
    querySelector(selector) { return connectionSelectors.get(selector) ?? null; },
    querySelectorAll(selector) {
      if (selector === "[data-tab]") return tabs;
      if (selector === "[data-panel]") return panels;
      if (selector === '[data-action="toggle-connections"]') return [connectionSummary, connectionTrigger];
      return [];
    },
  };
  const window = {
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    matchMedia: () => ({ matches: false }),
    addEventListener() {},
    setInterval() {},
  };
  const clickTab = (tab) => onClick({
    target: { closest: (selector) => selector === "[data-tab]" ? tab : null },
  });

  await runApp(source, { document, window, URL, Intl });
  onClick({ target: connectionSummary });
  assert.equal(connectionSummary.attributes.get("aria-expanded"), "true");
  assert.equal(connectionTrigger.attributes.get("aria-expanded"), "true");

  clickTab(tabs[1]);
  assert.equal(connectionSummary.attributes.get("aria-expanded"), "true");
  assert.equal(connectionTrigger.attributes.get("aria-expanded"), "true");
  clickTab(tabs[2]);
  assert.equal(connectionSummary.attributes.get("aria-expanded"), "true");
  assert.equal(connectionTrigger.attributes.get("aria-expanded"), "true");
  clickTab(tabs[1]);

  assert.equal(panels[0].hidden, true);
  assert.equal(panels[1].hidden, false);
  assert.equal(panels[1].animations.length, 2);
  assert.equal(panels[1].animations[0].keyframes[0].transform, "translate3d(10px, 0, 0)");
  assert.equal(panels[1].animations[0].options.duration, 180);
  assert.equal(panels[1].animations[0].cancelled, true);
  assert.equal(panels[1].animations[1].keyframes[0].transform, "translate3d(-10px, 0, 0)");
  assert.equal(panels[1].animations[1].keyframes[0].filter, "blur(3px)");

  clickTab(tabs[0]);
  assert.equal(connectionSummary.attributes.get("aria-expanded"), "true");
  assert.equal(connectionTrigger.attributes.get("aria-expanded"), "true");
  assert.equal(panels[0].animations.length, 0);
  assert.equal(liveSurface.animations.length, 1);
  assert.equal(liveSurface.animations[0].keyframes[0].transform, "translate3d(-10px, 0, 0)");
});

test("减少动态效果时页面切换不播放动画", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const tabs = [element({ tab: "live" }), element({ tab: "statistics" })];
  const panels = [motionElement({ panel: "live" }), motionElement({ panel: "statistics" }, true)];
  let onClick;
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener(type, handler) { if (type === "click") onClick = handler; },
    querySelectorAll(selector) {
      if (selector === "[data-tab]") return tabs;
      if (selector === "[data-panel]") return panels;
      return [];
    },
  };
  const window = {
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    matchMedia: () => ({ matches: true }),
    addEventListener() {},
    setInterval() {},
  };

  await runApp(source, { document, window, URL, Intl });
  onClick({ target: { closest: (selector) => selector === "[data-tab]" ? tabs[1] : null } });

  assert.equal(panels[1].hidden, false);
  assert.equal(panels[1].animations.length, 0);
});

test("条件区域从隐藏变为可见时播放轻量反馈", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const installUpdate = motionElement({ visible: "install-update" }, true);
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelectorAll(selector) {
      if (selector === "[data-visible]") return [installUpdate];
      return [];
    },
  };
  const window = {
    __TAURI__: { core: { invoke: async () => ({ updateState: "available" }) } },
    location: { href: "tauri://localhost/?tab=config" },
    history: { replaceState() {} },
    matchMedia: () => ({ matches: false }),
    addEventListener() {},
    setInterval() {},
  };

  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(installUpdate.hidden, false);
  assert.equal(installUpdate.animations.length, 1);
  assert.equal(installUpdate.animations[0].options.duration, 140);
  assert.deepEqual(
    Array.from(installUpdate.animations[0].keyframes, (frame) => frame.opacity),
    [0, 1],
  );
});

test("最新版检查完成后显示已检查和 Turbo 已是最新", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  let onClick;
  const checkUpdate = element({ action: "check-for-updates" });
  checkUpdate.closest = (selector) => selector === "[data-action]" ? checkUpdate : null;
  const updateState = element({ state: "update-state" });
  const updateMessage = element({ state: "update-message" });
  const document = {
    hidden: false,
    readyState: "complete",
    body: element(),
    addEventListener(type, handler) {
      if (type === "click") onClick = handler;
    },
    querySelector() { return null; },
    querySelectorAll(selector) {
      if (selector === "[data-action]") return [checkUpdate];
      if (selector === "[data-state]") return [updateState, updateMessage];
      return [];
    },
  };
  const window = {
    __TAURI__: { core: { invoke(command) {
      if (command === "check_for_updates") {
        return Promise.resolve({ updateState: "current", updateMessage: "Turbo 已是最新", updateProgress: 100 });
      }
      return Promise.resolve({ updateState: "idle", updateMessage: "尚未检查更新", updateProgress: 0 });
    } } },
    location: { href: "tauri://localhost/?tab=config" },
    history: { replaceState() {} },
    matchMedia: () => ({ matches: false }),
    addEventListener() {},
    setInterval() {},
  };

  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));
  onClick({ target: checkUpdate });
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(updateState.textContent, "已检查");
  assert.equal(updateMessage.textContent, "Turbo 已是最新");
});

test("安装更新期间持续读取并展示下载进度", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  let status = {
    updateState: "available",
    updateMessage: "发现新版本 v0.1.0-beta.10",
    updateProgress: 0,
  };
  let statusPolls = 0;
  let onClick;
  let onTick;
  const installUpdate = element({ action: "install-update", visible: "install-update" });
  installUpdate.closest = (selector) => selector === "[data-action]" ? installUpdate : null;
  const updateState = element({ state: "update-state" });
  const updateMessage = element({ state: "update-message" });
  const updatePercent = motionElement({ state: "update-progress", visible: "update-progress" }, true);
  const progressTrack = motionElement({ visible: "update-progress" }, true);
  const progressFill = element({ stateProgress: "update-progress" });
  let progressScale = "";
  progressFill.style.setProperty = (name, value) => {
    if (name === "--progress") progressScale = value;
  };
  const document = {
    hidden: false,
    readyState: "complete",
    body: element(),
    addEventListener(type, handler) {
      if (type === "click") onClick = handler;
    },
    querySelector() { return null; },
    querySelectorAll(selector) {
      if (selector === "[data-action]") return [installUpdate];
      if (selector === "[data-state]") return [updateState, updateMessage, updatePercent];
      if (selector === "[data-state-progress]") return [progressFill];
      if (selector === "[data-visible]") return [installUpdate, updatePercent, progressTrack];
      if (selector === '[role="progressbar"]') return [progressTrack];
      return [];
    },
  };
  const window = {
    __TAURI__: { core: { invoke(command) {
      if (command === "install_update") return new Promise(() => {});
      if (command === "get_app_status") {
        statusPolls += 1;
        return Promise.resolve(status);
      }
      return Promise.resolve({});
    } } },
    location: { href: "tauri://localhost/?tab=config" },
    history: { replaceState() {} },
    matchMedia: () => ({ matches: false }),
    addEventListener() {},
    setInterval(handler) { onTick = handler; },
  };

  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));
  onClick({ target: installUpdate });
  await new Promise((resolve) => setImmediate(resolve));

  status = {
    updateState: "downloading",
    updateMessage: "正在下载签名更新",
    updateProgress: 68,
  };
  await onTick();

  assert.equal(statusPolls, 2);
  assert.equal(updateState.textContent, "下载中");
  assert.equal(updateMessage.textContent, "正在下载签名更新");
  assert.equal(updatePercent.textContent, "68%");
  assert.equal(updatePercent.hidden, false);
  assert.equal(progressTrack.hidden, false);
  assert.equal(progressTrack.attributes.get("aria-valuenow"), "68");
  assert.equal(progressScale, "0.68");
  assert.equal(installUpdate.textContent, "更新中");
  assert.equal(installUpdate.disabled, true);
  assert.equal(installUpdate.attributes.get("aria-busy"), "true");
});

test("实时终端只增量追加新请求并同步触发传输脉冲", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  let status = {
    serviceHealthy: true,
    configState: "managed",
    recentRequests: [
      { id: 1, timestampMs: 1_000, status: 200, path: "/v1/responses", rawBytes: 100, sentBytes: 60, transport: "HTTP", result: "success" },
    ],
    trafficWindows: [],
  };
  let markup = "";
  let fullWrites = 0;
  const appends = [];
  const requestStream = element();
  Object.defineProperty(requestStream, "innerHTML", {
    get() { return markup; },
    set(value) { markup = value; fullWrites += 1; },
  });
  requestStream.insertAdjacentHTML = (_position, value) => {
    markup += value;
    appends.push(value);
  };
  const terminal = { scrollHeight: 100, scrollTop: 0, clientHeight: 100, addEventListener() {} };
  const selectors = new Map([
    ["[data-request-stream]", requestStream],
    [".c-terminal__window", terminal],
  ]);
  let onTick;
  const document = {
    hidden: false,
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelector(selector) { return selectors.get(selector) ?? null; },
    querySelectorAll() { return []; },
  };
  const pulses = [];
  const window = {
    __TAURI__: { core: { invoke: async () => status } },
    TurboStrands: { setCount() {}, pulse(value) { pulses.push(value); } },
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval(handler) { onTick = handler; },
  };

  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));
  const writesAfterHydration = fullWrites;
  await onTick();

  assert.equal(fullWrites, writesAfterHydration);
  assert.equal(appends.length, 0);
  assert.equal(pulses.length, 0);

  status = {
    ...status,
    recentRequests: [
      ...status.recentRequests,
      { id: 2, timestampMs: 2_000, status: 200, path: "/v1/responses", rawBytes: 120, sentBytes: 60, transport: "WS", result: "success" },
    ],
  };
  await onTick();

  assert.equal(fullWrites, writesAfterHydration);
  assert.equal(appends.length, 1);
  assert.match(appends[0], /class="c-request-row is-new"/);
  assert.match(appends[0], /data-request-id="2"/);
  assert.equal(pulses.length, 1);
  assert.equal(pulses[0], 0.5);
});

test("相同状态轮询不改变实时终端的滚动位置", async () => {
  const live = await liveTailHarness();
  assert.equal(live.terminal.scrollTop, live.terminal.scrollHeight);

  live.scrollTo(176);
  await live.tick();
  assert.equal(live.terminal.scrollTop, 176);
  assert.equal(live.follow.hidden, true);

  live.scrollTo(175);
  await live.tick();
  assert.equal(live.terminal.scrollTop, 175);
  assert.equal(live.follow.hidden, false);
});

test("上滚后继续追加请求并提供回到最新操作", async () => {
  const live = await liveTailHarness();
  live.scrollTo(80);
  assert.equal(live.follow.hidden, false);
  assert.equal(live.followLabel.textContent, "回到最新");

  live.setRequests([
    { id: 1, timestampMs: 1_000, status: 200, path: "/v1/responses", rawBytes: 100, sentBytes: 60, transport: "HTTP", result: "success" },
    { id: 2, timestampMs: 2_000, status: 200, path: "/v1/responses", rawBytes: 120, sentBytes: 60, transport: "WS", result: "success" },
  ]);
  await live.tick();

  assert.equal(live.terminal.scrollTop, 80);
  assert.equal(live.followLabel.textContent, "1 条新请求");
  await live.click("follow-live");
  assert.equal(live.terminal.scrollTop, live.terminal.scrollHeight);
  assert.equal(live.follow.hidden, true);
  assert.equal(live.followLabel.textContent, "回到最新");
});

test("仅真实发布排空的空闲 1012 显示发布重建", async () => {
  const live = await liveTailHarness();
  live.setRequests([
    { id: 2, timestampMs: 2_000, status: 1012, path: "/v1/responses", rawBytes: 0, sentBytes: 0, transport: "WS", result: "error", route: "hybridWs", failurePhase: "hybridIdle", failureReason: "service restarting" },
    { id: 3, timestampMs: 3_000, status: 1012, path: "/v1/responses", rawBytes: 0, sentBytes: 0, transport: "WS", result: "error", route: "hybridWs", failurePhase: "hybridIdle", failureReason: "upstream requires HTTP replay" },
  ]);

  await live.tick();

  assert.equal(live.requestStream.innerHTML.match(/Hybrid WS · 发布重建/gu)?.length, 1);
  assert.equal(live.requestStream.innerHTML.match(/Hybrid WS · 连接恢复/gu)?.length, 1);
});

test("手动滚到底部或清空终端会恢复跟随", async () => {
  const live = await liveTailHarness();
  live.scrollTo(80);
  live.scrollTo(live.terminal.scrollHeight - live.terminal.clientHeight);
  assert.equal(live.follow.hidden, true);

  live.scrollTo(80);
  await live.click("clear-stream");
  assert.equal(live.follow.hidden, true);
  assert.equal(live.followLabel.textContent, "回到最新");
});

test("实时终端提供可访问的回到最新操作", async () => {
  const html = await readFile(new URL("../src/index.html", import.meta.url), "utf8");

  assert.match(html, /data-action="follow-live"[^>]*data-live-follow[^>]*hidden/);
  assert.match(html, /data-live-follow-label[^>]*aria-live="polite"[^>]*>回到最新</);
});

test("页面隐藏时暂停状态与连接摘要轮询并在恢复可见后立即刷新", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const invokes = [];
  let onTick;
  let onVisibilityChange;
  const document = {
    hidden: false,
    readyState: "complete",
    body: element(),
    addEventListener(type, handler) {
      if (type === "visibilitychange") onVisibilityChange = handler;
    },
    querySelectorAll() { return []; },
  };
  const window = {
    __TAURI__: { core: { invoke: async (command) => {
      invokes.push(command);
      return command === "get_connection_snapshot"
        ? { currentConnections: 0, prewarm: 0, boundThreads: [], transitions: [], recentClosed: [] }
        : {};
    } } },
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval(handler) { onTick = handler; },
  };

  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(invokes, ["get_app_status", "get_connection_snapshot"]);

  document.hidden = true;
  await onTick();
  assert.deepEqual(invokes, ["get_app_status", "get_connection_snapshot"]);

  document.hidden = false;
  onVisibilityChange();
  await new Promise((resolve) => setImmediate(resolve));
  assert.deepEqual(invokes, [
    "get_app_status",
    "get_connection_snapshot",
    "get_app_status",
    "get_connection_snapshot",
  ]);
});
