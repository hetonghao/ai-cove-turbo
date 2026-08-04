import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

function element(extra = {}) {
  const style = {
    values: {},
    setProperty(name, value) {
      this.values[name] = value;
    },
  };
  return {
    dataset: {},
    hidden: false,
    innerHTML: "",
    style,
    textContent: "",
    setAttribute() {},
    ...extra,
  };
}

test("统计图按时间范围切换颗粒度并提供时间桶详情", async () => {
  // Given: 统计页的真实原型脚本和最小 DOM 契约。
  const source = await readFile(new URL("../prototype/app.js", import.meta.url), "utf8");
  let now = new Date(2026, 7, 4, 15, 37, 47).getTime();
  class FixedDate extends Date {
    constructor(...args) {
      super(...(args.length ? args : [now]));
    }

    static now() {
      return now;
    }
  }
  const range = element({ value: "1440" });
  const bars = element();
  const axis = element();
  const granularity = element();
  const windows = element();
  const empty = element();
  let onChange;
  let onTick;
  const selectors = {
    '[data-filter="range"]': range,
    '[data-filter="transport"]': element({ value: "all" }),
    '[data-filter="result"]': element({ value: "all" }),
    "[data-stat-bars]": bars,
    "[data-stat-axis]": axis,
    "[data-stat-granularity]": granularity,
    "[data-stat-windows]": windows,
    "[data-stat-empty]": empty,
  };
  const document = {
    readyState: "complete",
    body: element(),
    addEventListener(type, handler) {
      if (type === "change") onChange = handler;
    },
    querySelector(selector) {
      return selectors[selector] ?? null;
    },
    querySelectorAll() {
      return [];
    },
  };
  const window = {
    location: { href: "http://127.0.0.1:4174/?tab=statistics" },
    history: { replaceState() {} },
    addEventListener() {},
    setInterval(handler) {
      onTick = handler;
    },
  };

  // When: 页面初始化并依次选择三个时间范围。
  vm.runInNewContext(source, { Date: FixedDate, document, Intl, Math, Object, Set, URL, window });

  // Then: x 轴覆盖起止时间，粒度和桶数随范围变化，每个桶可读出四项详情。
  assert.equal((axis.innerHTML.match(/<span/g) ?? []).length, 3);
  assert.match(axis.innerHTML, /^<span>[^<]*:00<\/span>/);
  assert.equal(granularity.textContent, "每 1 小时");
  assert.equal((bars.innerHTML.match(/class="c-bar-slot/g) ?? []).length, 24);
  assert.equal(bars.style.values["--chart-bucket-count"], "24");
  assert.match(axis.innerHTML, /<span>[^<]*15:00<\/span>$/);
  assert.match(bars.innerHTML, /role="tooltip"[\s\S]*时间[\s\S]*请求数[\s\S]*发送[\s\S]*节省/);

  range.value = "10";
  onChange({ target: { matches: () => true } });
  assert.equal(granularity.textContent, "每 1 分钟");
  assert.equal((bars.innerHTML.match(/class="c-bar-slot/g) ?? []).length, 10);

  range.value = "1";
  onChange({ target: { matches: () => true } });
  assert.equal(granularity.textContent, "每 10 秒");
  assert.equal((bars.innerHTML.match(/class="c-bar-slot/g) ?? []).length, 6);
  assert.equal(bars.style.values["--chart-bucket-count"], "6");
  assert.match(axis.innerHTML, /^<span>[^<]*:50<\/span>/);
  assert.match(axis.innerHTML, /<span>[^<]*:40<\/span>$/);

  // When: 新请求到达时已经跨过一个 10 秒周期。
  now += 4_000;
  onTick();

  // Then: 最旧周期退出、当前周期追加到最右侧，新请求计入当前周期。
  assert.equal((bars.innerHTML.match(/class="c-bar-slot/g) ?? []).length, 6);
  assert.match(axis.innerHTML, /^<span>[^<]*:00<\/span>/);
  assert.match(axis.innerHTML, /<span>[^<]*:50<\/span>$/);
  assert.match(bars.innerHTML, /id="chart-tooltip-5"[\s\S]*请求数<\/strong><span>1<\/span>/);
});
