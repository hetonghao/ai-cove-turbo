import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

test("Strands 使用一套 WebGL bloom 着色器驱动多个 Canvas", async () => {
  // Given: 配置页和实时页各有一个支持 WebGL2 的 Canvas。
  const source = await readFile(new URL("../src/strands.js", import.meta.url), "utf8");
  const shaderSources = [];
  const uniformCounts = [[], []];
  const draws = [0, 0];
  const createGl = (index) => ({
      VERTEX_SHADER: 1, FRAGMENT_SHADER: 2, COMPILE_STATUS: 3, LINK_STATUS: 4,
      ARRAY_BUFFER: 5, STATIC_DRAW: 6, FLOAT: 7, TRIANGLES: 8,
      COLOR_BUFFER_BIT: 9, BLEND: 10, ONE: 11, ONE_MINUS_SRC_ALPHA: 12,
      createShader() { return {}; },
      shaderSource(_shader, shaderSource) { shaderSources.push(shaderSource); },
      compileShader() {}, getShaderParameter() { return true; }, getShaderInfoLog() { return ""; },
      createProgram() { return {}; }, attachShader() {}, linkProgram() {}, getProgramParameter() { return true; }, getProgramInfoLog() { return ""; },
      createBuffer() { return {}; }, bindBuffer() {}, bufferData() {},
      getAttribLocation() { return 0; }, enableVertexAttribArray() {}, vertexAttribPointer() {},
      getUniformLocation(_program, name) { return name; },
      uniform1f() {}, uniform2f() {}, uniform2fv() {}, uniform3fv() {},
      uniform1i(name, value) { if (name === "uStrandCount") uniformCounts[index].push(value); },
      clearColor() {}, enable() {}, blendFunc() {}, viewport() {}, clear() {}, useProgram() {},
      drawArrays() { draws[index] += 1; },
    });
  const canvases = [0, 1].map((index) => ({
    dataset: { count: "2" },
    getContext(kind) { assert.equal(kind, "webgl2"); return createGl(index); },
    getBoundingClientRect() { return { width: 150, height: 78 }; },
  }));
  const window = {
    devicePixelRatio: 1,
    matchMedia() { return { matches: true, addEventListener() {} }; },
    getComputedStyle() {
      const values = {
        "--turbo-strands-orange": "#f97316",
        "--turbo-strands-violet": "#7c3aed",
        "--turbo-strands-cyan": "#06b6d4",
      };
      return { getPropertyValue(name) { return values[name] ?? ""; } };
    },
    addEventListener() {},
  };
  const document = { querySelectorAll() { return canvases; } };

  // When: 两个组件首次按 count=2 绘制。
  // Then: 两个 GPU 上下文都接收相同状态数量。
  assert.doesNotThrow(() => vm.runInNewContext(source, { document, Math, Number, window }));
  assert.ok(shaderSources.some((shader) => shader.includes("col = 1.0 - exp(-col * uGlow)")));
  assert.deepEqual(uniformCounts.map((counts) => counts.at(-1)), [2, 2]);
  window.TurboStrands.setCount(9);
  assert.deepEqual(uniformCounts.map((counts) => counts.at(-1)), [5, 5]);
  assert.ok(draws.every((count) => count > 0));
});

test("Strands 根据新请求提高能量并在页面隐藏时停止动画帧", async () => {
  const source = await readFile(new URL("../src/strands.js", import.meta.url), "utf8");
  const intensities = [];
  const frames = new Map();
  const cancelled = [];
  const listeners = {};
  let nextFrame = 1;
  const gl = {
    VERTEX_SHADER: 1, FRAGMENT_SHADER: 2, COMPILE_STATUS: 3, LINK_STATUS: 4,
    ARRAY_BUFFER: 5, STATIC_DRAW: 6, FLOAT: 7, TRIANGLES: 8,
    COLOR_BUFFER_BIT: 9, BLEND: 10, ONE: 11, ONE_MINUS_SRC_ALPHA: 12,
    createShader() { return {}; }, shaderSource() {}, compileShader() {}, getShaderParameter() { return true; }, getShaderInfoLog() { return ""; },
    createProgram() { return {}; }, attachShader() {}, linkProgram() {}, getProgramParameter() { return true; }, getProgramInfoLog() { return ""; },
    createBuffer() { return {}; }, bindBuffer() {}, bufferData() {}, getAttribLocation() { return 0; }, enableVertexAttribArray() {}, vertexAttribPointer() {},
    getUniformLocation(_program, name) { return name; },
    uniform1f(name, value) { if (name === "uIntensity") intensities.push(value); },
    uniform1i() {}, uniform2f() {}, uniform2fv() {}, uniform3fv() {}, clearColor() {}, enable() {}, blendFunc() {}, viewport() {}, clear() {}, useProgram() {}, drawArrays() {},
  };
  const canvas = {
    dataset: { count: "2" },
    getContext() { return gl; },
    getBoundingClientRect() { return { width: 150, height: 78 }; },
  };
  const document = {
    hidden: false,
    querySelectorAll() { return [canvas]; },
    addEventListener(type, handler) { listeners[type] = handler; },
  };
  const window = {
    devicePixelRatio: 1,
    matchMedia() { return { matches: false, addEventListener() {} }; },
    getComputedStyle() {
      const values = { "--turbo-strands-orange": "#f97316", "--turbo-strands-violet": "#7c3aed", "--turbo-strands-cyan": "#06b6d4" };
      return { getPropertyValue(name) { return values[name] ?? ""; } };
    },
    requestAnimationFrame(callback) { const id = nextFrame; nextFrame += 1; frames.set(id, callback); return id; },
    cancelAnimationFrame(id) { cancelled.push(id); frames.delete(id); },
    addEventListener() {},
  };
  const runFrame = (time) => {
    const [id, callback] = frames.entries().next().value;
    frames.delete(id);
    callback(time);
  };

  vm.runInNewContext(source, { document, Math, Number, window });
  runFrame(16);
  window.TurboStrands.pulse(1);
  runFrame(32);

  assert.ok(intensities.some((value) => value > 0.4));

  document.hidden = true;
  listeners.visibilitychange();
  assert.ok(cancelled.length > 0);
  assert.equal(frames.size, 0);

  document.hidden = false;
  listeners.visibilitychange();
  assert.equal(frames.size, 1);
});


async function createTimelineHarness() {
  const source = await readFile(new URL("../src/strands.js", import.meta.url), "utf8");
  const uniforms = new Map();
  const frames = new Map();
  const listeners = {};
  let nextFrame = 1;
  let fragmentShader = "";
  let bounds = { width: 220, height: 144 };
  const gl = {
    VERTEX_SHADER: 1, FRAGMENT_SHADER: 2, COMPILE_STATUS: 3, LINK_STATUS: 4,
    ARRAY_BUFFER: 5, STATIC_DRAW: 6, FLOAT: 7, TRIANGLES: 8,
    COLOR_BUFFER_BIT: 9, BLEND: 10, ONE: 11, ONE_MINUS_SRC_ALPHA: 12,
    createShader(type) { return { type }; },
    shaderSource(shader, text) { if (shader.type === this.FRAGMENT_SHADER) fragmentShader = text; },
    compileShader() {}, getShaderParameter() { return true; }, getShaderInfoLog() { return ""; },
    createProgram() { return {}; }, attachShader() {}, linkProgram() {}, getProgramParameter() { return true; }, getProgramInfoLog() { return ""; },
    createBuffer() { return {}; }, bindBuffer() {}, bufferData() {}, getAttribLocation() { return 0; }, enableVertexAttribArray() {}, vertexAttribPointer() {},
    getUniformLocation(_program, name) { return name; },
    uniform1f(name, value) { uniforms.set(name, Math.fround(value)); },
    uniform2fv(name, values) { uniforms.set(name, Array.from(values, Math.fround)); },
    uniform1i() {}, uniform2f() {}, uniform3fv() {}, clearColor() {}, enable() {}, blendFunc() {}, viewport() {}, clear() {}, useProgram() {}, drawArrays() {},
  };
  const canvas = {
    dataset: { count: "5" },
    getContext() { return gl; },
    getBoundingClientRect() { return bounds; },
  };
  const document = {
    hidden: false,
    querySelectorAll() { return [canvas]; },
    addEventListener(type, handler) { listeners[type] = handler; },
  };
  const window = {
    devicePixelRatio: 2,
    matchMedia() { return { matches: false, addEventListener() {} }; },
    getComputedStyle() { return { getPropertyValue() { return "#f97316"; } }; },
    requestAnimationFrame(callback) { const id = nextFrame++; frames.set(id, callback); return id; },
    cancelAnimationFrame(id) { frames.delete(id); },
    addEventListener(type, handler) { listeners[type] = handler; },
  };
  vm.runInNewContext(source, { document, Math, Number, window });
  return {
    canvas, frames, fragmentShader, window,
    drawAt(time) {
      assert.equal(frames.size, 1, "只应有一条动画帧链");
      const [id, callback] = frames.entries().next().value;
      frames.delete(id);
      callback(time);
      return { phases: uniforms.get("uWavePhases[0]"), hue: uniforms.get("uHuePhase"), time: uniforms.get("uTime"), speed: uniforms.get("uSpeed") };
    },
    setVisible(visible) {
      document.hidden = !visible;
      listeners.visibilitychange();
    },
    resize(width, height) {
      bounds = { width, height };
      listeners.resize();
    },
  };
}

const TAU = Math.PI * 2;
const phaseDistance = (a, b, period) => Math.min(Math.abs(a - b), period - Math.abs(a - b));

function assertPeriodicState(state, milliseconds) {
  assert.equal(state.phases?.length, 10, "双频相位必须在进入 GPU 前独立归一化");
  const seconds = milliseconds * 0.001;
  state.phases.forEach((phase, index) => {
    const velocity = 1.4 + Math.floor(index / 2) * 1.2;
    const angle = seconds * 0.5 * velocity;
    const expected = (index % 2 ? angle * 0.7 : angle) % TAU;
    assert.ok(phase >= 0 && phase <= Math.fround(TAU), "波形相位必须保持有界");
    assert.ok(phaseDistance(phase, expected, TAU) < 0.000001, "每条光带的双频速度必须保持不变");
  });
  assert.ok(state.hue >= 0 && state.hue <= 1, "颜色相位必须保持有界");
  assert.ok(phaseDistance(state.hue, (seconds * 0.04) % 1, 1) < 0.000001, "颜色速度独立于波形速度");
}

function sampleGpuWave(uvX, strand, phases) {
  const f = Math.fround;
  const frequency = f(2 + f(strand * f(0.35)));
  const phase = f(strand * f(1.7));
  const a = f(f(f(f(uvX) * frequency) + phases[strand * 2]) + phase);
  const b = f(f(f(f(f(uvX) * frequency) * f(1.1)) - phases[strand * 2 + 1]) + f(phase * f(1.7)));
  return f(f(f(Math.sin(a)) * f(0.60)) + f(f(Math.sin(b)) * f(0.40)));
}

test("Strands 在运行 1、11、30、365 天后仍保留相邻像素的波形精度", async () => {
  const harness = await createTimelineHarness();
  for (const time of [16, 3600016, 86400016, 950400016, 2592000016, 31536000016]) {
    const state = harness.drawAt(time);
    const f = Math.fround;
    const frequency = f(2 + f(4 * f(0.35)));
    // 同时模拟旧版累计时间和新版相位入口，让此断言直接捕获像素阶梯而非接口差异。
    const timePhase = state.phases?.[8] ?? f(f(state.time * state.speed) * f(1.4 + f(4 * f(1.2))));
    let previous;
    for (let x = 0; x < harness.canvas.width; x += 1) {
      const uvX = f(f(f(x + 0.5) - f(harness.canvas.width * 0.5)) / f(harness.canvas.width / f(1.9)) / f(1.875));
      const argument = f(f(f(uvX * frequency) + timePhase) + f(4 * f(1.7)));
      assert.notEqual(argument, previous, "累计运行时间不能把相邻像素吞并成阶梯");
      previous = argument;
    }
  }
  assert.doesNotMatch(harness.fragmentShader, /\buTime\b/, "着色器不应再接收无界累计时间");
  assert.match(harness.fragmentShader, /uWavePhases\[i\]/);
  assert.match(harness.fragmentShader, /uHuePhase/);
});

test("Strands 有界相位保持原双频波形且跨周期不突跳", async () => {
  const harness = await createTimelineHarness();
  const centers = [25, TAU * 100000 / 3.1, TAU * 100000 / (3.1 * 0.7), 950400];
  for (const center of centers) {
    const before = harness.drawAt((center - 0.00025) * 1000);
    const after = harness.drawAt((center + 0.00025) * 1000);
    for (const [state, seconds] of [[before, center - 0.00025], [after, center + 0.00025]]) {
      assertPeriodicState(state, seconds * 1000);
      for (let strand = 0; strand < 5; strand += 1) {
        for (const uvX of [-0.5, -0.125, 0, 0.125, 0.5]) {
          const frequency = 2 + strand * 0.35;
          const phase = strand * 1.7;
          const angle = seconds * 0.5 * (1.4 + strand * 1.2);
          const expected = Math.sin(uvX * frequency + angle + phase) * 0.60
            + Math.sin(uvX * frequency * 1.1 - angle * 0.7 + phase * 1.7) * 0.40;
          assert.ok(Math.abs(sampleGpuWave(uvX, strand, state.phases) - expected) < 0.00001, "归一化不得改变原波形");
        }
      }
    }
    assert.ok(phaseDistance(before.hue, after.hue, 1) < 0.0001, "颜色跨周期必须连续");
    assert.notEqual(before.phases[8], after.phases[8], "长时间运行后仍应响应亚毫秒时间变化");
    for (let strand = 0; strand < 5; strand += 1) {
      assert.ok(Math.abs(sampleGpuWave(0.125, strand, before.phases) - sampleGpuWave(0.125, strand, after.phases)) < 0.002, "波形跨周期必须连续");
    }
  }
});

test("Strands 长期隐藏后恢复、缩放和状态变化不丢失相位精度", async () => {
  const harness = await createTimelineHarness();
  const started = 950400016;
  assertPeriodicState(harness.drawAt(started), started);
  harness.setVisible(false);
  assert.equal(harness.frames.size, 0);
  harness.resize(0, 0);
  harness.resize(96, 64);
  harness.setVisible(true);
  harness.window.TurboStrands.setCount(3);
  harness.window.TurboStrands.pulse(1);
  const resumed = 2592000016;
  assertPeriodicState(harness.drawAt(resumed), resumed);
  assert.equal(harness.canvas.width, 192);
  assert.equal(harness.canvas.height, 128);
  assert.equal(harness.frames.size, 1);
});
