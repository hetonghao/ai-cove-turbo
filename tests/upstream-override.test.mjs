import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

function element(dataset = {}) {
  const attributes = new Map();
  const classes = new Set();
  return {
    dataset,
    attributes,
    hidden: false,
    open: false,
    value: "",
    textContent: "",
    style: { setProperty() {} },
    classList: {
      add(...names) { names.forEach((name) => classes.add(name)); },
      remove(...names) { names.forEach((name) => classes.delete(name)); },
      toggle(name, force) {
        const next = force === undefined ? !classes.has(name) : Boolean(force);
        if (next) classes.add(name); else classes.delete(name);
        return next;
      },
      contains(name) { return classes.has(name); },
    },
    setAttribute(name, value) { attributes.set(name, String(value)); },
    getAttribute(name) { return attributes.get(name) ?? null; },
    removeAttribute(name) { attributes.delete(name); },
    matches() { return false; },
    closest(selector) { return selector === "[data-upstream-trigger]" ? this : null; },
    focus() {},
    select() {},
  };
}

async function upstreamHarness({ aiCove = true } = {}) {
  const source = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const telemetrySource = await readFile(new URL("../src/telemetry.js", import.meta.url), "utf8");
  const connectionDomSource = await readFile(new URL("../src/connection-dom.js", import.meta.url), "utf8");
  const trigger = element({ upstreamTrigger: "" });
  const upstream = element({ state: "upstream" });
  const nonAiMarker = element({ upstreamNonAi: "" });
  const dialog = element({ upstreamDialog: "" });
  const field = element({ upstreamField: "" });
  const error = element({ upstreamError: "" });
  const candidates = element({ upstreamCandidates: "" });
  const fixed = element({ upstreamCandidate: "https://api.ai-cove.com/v1" });
  const original = element({ upstreamCandidate: "https://example.com/v1" });
  const save = element({ action: "save-upstream" });
  const cancel = element({ upstreamDialogClose: "" });
  const state = {
    serviceHealthy: true,
    configState: "managed",
    configMessage: "配置已生效",
    endpoint: "http://127.0.0.1:44175/v1",
    upstream: aiCove ? "https://api.ai-cove.com/v1" : "https://example.com/v1",
    originalUpstream: aiCove ? "https://api.ai-cove.com/v1" : "https://example.com/v1",
    aiCoveUpstream: aiCove,
    compressionEnabled: true,
    websocketEnabled: true,
    compressionVerified: true,
    websocketVerified: true,
    websocketZstdVerified: true,
    websocketState: "connected",
    provider: "custom",
    codexState: "active",
    recentRequests: [],
    trafficWindows: [],
    sessionNames: {},
    catalog: { models: [] },
    modelPolicy: { models: {}, defaultTransport: "auto" },
  };
  const calls = [];
  const listeners = new Map();
  const invoke = async (command, args) => {
    calls.push({ command, args });
    if (command === "get_app_status") return { ...state };
    if (command === "set_upstream_override") {
      state.upstream = args.upstream;
      state.aiCoveUpstream = args.upstream.toLowerCase().includes(".ai-cove.com");
      return { ...state };
    }
    return { ...state };
  };
  const selectors = new Map([
    ["[data-upstream-trigger]", trigger],
    ["[data-state=\"upstream\"]", upstream],
    ["[data-upstream-non-ai]", nonAiMarker],
    ["[data-upstream-dialog]", dialog],
    ["[data-upstream-field]", field],
    ["[data-upstream-error]", error],
    ["[data-upstream-candidates]", candidates],
    ["[data-upstream-candidate=\"https://api.ai-cove.com/v1\"]", fixed],
    ["[data-upstream-candidate=\"https://example.com/v1\"]", original],
    ["[data-action=\"save-upstream\"]", save],
    ["[data-upstream-dialog-close]", cancel],
  ]);
  const document = {
    hidden: false,
    readyState: "complete",
    body: element(),
    addEventListener(type, handler) { listeners.set(type, handler); },
    querySelector(selector) { return selectors.get(selector) ?? null; },
    querySelectorAll(selector) {
      if (selector === "[data-state]") return [upstream];
      if (selector === "[data-action]") return [save];
      if (selector === "[data-upstream-candidate]") return [fixed, original];
      return [];
    },
    getElementById() { return null; },
  };
  dialog.querySelector = (selector) => selectors.get(selector) ?? null;
  dialog.showModal = () => { dialog.open = true; };
  dialog.close = () => { dialog.open = false; };
  fixed.closest = (selector) => selector === "[data-upstream-candidate]" ? fixed : null;
  original.closest = (selector) => selector === "[data-upstream-candidate]" ? original : null;
  save.closest = (selector) => selector === "[data-action]" || selector === '[data-action="save-upstream"]' ? save : null;
  cancel.closest = (selector) => selector === "[data-upstream-dialog-close]" ? cancel : null;
  const window = {
    __TAURI__: { core: { invoke } },
    location: { href: "tauri://localhost/?tab=live" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval() {},
    matchMedia: () => ({ matches: false, addEventListener() {} }),
    requestAnimationFrame(callback) { callback(); },
  };
  vm.runInNewContext(telemetrySource, { window, document, Intl, Date, URL, Error, Set, Map, JSON, Math, Number, String, Array, Object, RegExp });
  vm.runInNewContext(connectionDomSource, { window, document, Intl, Date, URL, Error, Set, Map, JSON, Math, Number, String, Array, Object, RegExp });
  vm.runInNewContext(source, { window, document, Intl, Date, URL, Error, Set, Map, JSON, Math, Number, String, Array, Object, RegExp });
  await new Promise((resolve) => setImmediate(resolve));
  return {
    elements: { trigger, upstream, nonAiMarker, dialog, field, error, candidates, fixed, original, save },
    calls,
    click(target, timeStamp = Date.now()) { listeners.get("click")?.({ target, timeStamp }); },
    keydown(target, key) { listeners.get("keydown")?.({ target, key, preventDefault() {} }); },
  };
}

test("UPSTREAM 三击打开隐蔽编辑弹窗，候选只填充并保存后调用覆盖命令", async () => {
  const harness = await upstreamHarness();
  const { trigger, dialog, field } = harness.elements;
  harness.click(trigger, 100);
  harness.click(trigger, 250);
  assert.equal(dialog.open, false);
  harness.click(trigger, 500);
  assert.equal(dialog.open, true);

  harness.click(harness.elements.original, 550);
  assert.equal(field.value, "https://example.com/v1");
  field.value = "https://gateway.example/v1";
  harness.click(harness.elements.save, 600);
  await new Promise((resolve) => setImmediate(resolve));

  assert.ok(harness.calls.some(({ command, args }) => command === "set_upstream_override"
    && args?.upstream === "https://gateway.example/v1"));
  assert.equal(dialog.open, false);
});

test("三次点击超过 600ms 不会打开上游弹窗", async () => {
  const harness = await upstreamHarness();
  harness.click(harness.elements.trigger, 100);
  harness.click(harness.elements.trigger, 701);
  harness.click(harness.elements.trigger, 702);
  assert.equal(harness.elements.dialog.open, false);
});

test("非 AI Cove 上游显示标记，AI Cove 上游不显示标记", async () => {
  const nonAi = await upstreamHarness({ aiCove: false });
  assert.equal(nonAi.elements.nonAiMarker.hidden, false);
  const aiCove = await upstreamHarness({ aiCove: true });
  assert.equal(aiCove.elements.nonAiMarker.hidden, true);
});

test("候选地址在原始上游与固定 AI Cove 相同的时候隐藏重复项", async () => {
  const same = await upstreamHarness({ aiCove: true });
  assert.doesNotMatch(same.elements.candidates.innerHTML, /接管前的原始 Codex 上游/);
  const different = await upstreamHarness({ aiCove: false });
  assert.match(different.elements.candidates.innerHTML, /接管前的原始 Codex 上游/);
});

test("实时页壳声明 UPSTREAM 覆盖入口、候选弹窗与非 AI Cove 标记", async () => {
  const html = await readFile(new URL("../src/index.html", import.meta.url), "utf8");
  const css = await readFile(new URL("../src/styles.css", import.meta.url), "utf8");
  const app = await readFile(new URL("../src/app.js", import.meta.url), "utf8");
  const rust = await readFile(new URL("../src-tauri/src/lib.rs", import.meta.url), "utf8");
  assert.match(html, /data-upstream-trigger/);
  assert.match(html, /data-upstream-dialog/);
  assert.match(html, /data-upstream-candidate="https:\/\/api\.ai-cove\.com\/v1"/);
  assert.match(html, /data-upstream-candidate="https:\/\/example\.com\/v1"/);
  assert.match(app, /set_upstream_override/);
  assert.match(rust, /set_upstream_override/);
  assert.match(css, /\.c-upstream-dialog/);
});

test("上游弹窗声明非零最小高度，避免原生 dialog 塌成一根线", async () => {
  const css = await readFile(new URL("../src/styles.css", import.meta.url), "utf8");
  const rule = css.match(/\.b-model-dialog\.c-upstream-dialog\s*\{([\s\S]*?)\n\}/)?.[1] || "";
  assert.match(rule, /min-block-size:\s*var\(--turbo-upstream-dialog-min-height\)/);
  const surfaceRule = css.match(/\.b-model-dialog\.c-upstream-dialog \.c-upstream-dialog__surface\s*\{([\s\S]*?)\n\}/)?.[1] || "";
  assert.match(surfaceRule, /grid-template-rows:\s*auto minmax\(0, 1fr\) auto auto/);
});
