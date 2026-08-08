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
  vm.runInNewContext(telemetrySource, context);
  vm.runInNewContext(source, context);
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

test("Codex 待重启时显示重启动作", async () => {
  // Given: 后端明确返回需要重启，页面里有一个复用的重启动作。
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const restart = element({ action: "restart-codex", required: "false" });
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener() {},
    querySelectorAll(selector) {
      if (selector === "[data-action]") return [restart];
      return [];
    },
  };
  const window = {
    __TAURI__: { core: { invoke: async () => ({ restartRequired: true }) } },
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval() {},
  };

  // When: 首次真实状态完成渲染。
  await runApp(source, { document, window, URL, Intl });
  await new Promise((resolve) => setImmediate(resolve));

  // Then: CSS 可据此显示手动重启按钮。
  assert.equal(restart.dataset.required, "true");
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

test("页面切换按方向进入且连续切换会中断旧动画", async () => {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const tabs = [element({ tab: "live" }), element({ tab: "statistics" }), element({ tab: "config" })];
  const panels = [
    motionElement({ panel: "live" }),
    motionElement({ panel: "statistics" }, true),
    motionElement({ panel: "config" }, true),
  ];
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
    matchMedia: () => ({ matches: false }),
    addEventListener() {},
    setInterval() {},
  };
  const clickTab = (tab) => onClick({
    target: { closest: (selector) => selector === "[data-tab]" ? tab : null },
  });

  await runApp(source, { document, window, URL, Intl });
  clickTab(tabs[1]);
  clickTab(tabs[2]);
  clickTab(tabs[1]);

  assert.equal(panels[0].hidden, true);
  assert.equal(panels[1].hidden, false);
  assert.equal(panels[1].animations.length, 2);
  assert.equal(panels[1].animations[0].keyframes[0].transform, "translate3d(10px, 0, 0)");
  assert.equal(panels[1].animations[0].options.duration, 180);
  assert.equal(panels[1].animations[0].cancelled, true);
  assert.equal(panels[1].animations[1].keyframes[0].transform, "translate3d(-10px, 0, 0)");
  assert.equal(panels[1].animations[1].keyframes[0].filter, "blur(3px)");
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
