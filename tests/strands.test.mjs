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
      uniform1f() {}, uniform2f() {}, uniform3fv() {},
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
    uniform1i() {}, uniform2f() {}, uniform3fv() {}, clearColor() {}, enable() {}, blendFunc() {}, viewport() {}, clear() {}, useProgram() {}, drawArrays() {},
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
