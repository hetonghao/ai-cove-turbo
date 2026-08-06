(() => {
  "use strict";

  // Adapted from React Bits Strands at pinned commit 1320d40a8318ac7d4fe6690c7206ceda8cdd59bd.
  // License: MIT + Commons Clause; see ../THIRD_PARTY_NOTICES.md.
  const MAX_STRANDS = 5;
  const MAX_COLORS = 3;
  const VERTEX_SHADER = `#version 300 es
in vec2 position;
void main() {
  gl_Position = vec4(position, 0.0, 1.0);
}
`;
  const FRAGMENT_SHADER = `#version 300 es
precision highp float;

uniform float uTime;
uniform vec2 uResolution;
uniform vec3 uColors[${MAX_COLORS}];
uniform int uColorCount;
uniform int uStrandCount;
uniform float uSpeed;
uniform float uAmplitude;
uniform float uWaviness;
uniform float uThickness;
uniform float uGlow;
uniform float uTaper;
uniform float uSpread;
uniform float uHueShift;
uniform float uIntensity;
uniform float uOpacity;
uniform float uScale;
uniform float uSaturation;

out vec4 fragColor;

const float PI = 3.14159265;

vec3 samplePalette(float t) {
  t = fract(t);
  float scaled = t * float(uColorCount);
  int idx = int(floor(scaled));
  float blend = fract(scaled);
  int nextIdx = idx + 1;
  if (nextIdx >= uColorCount) nextIdx = 0;
  return mix(uColors[idx], uColors[nextIdx], blend);
}

void main() {
  vec2 uv = (gl_FragCoord.xy - 0.5 * uResolution) / (uResolution.x / 1.9);
  uv /= max(uScale, 0.0001);

  float energy = 0.06 + uIntensity * 0.94;
  float envelope = pow(max(cos(uv.x * PI * 1.3), 0.0), uTaper);
  vec3 col = vec3(0.0);

  for (int i = 0; i < ${MAX_STRANDS}; i++) {
    if (i >= uStrandCount) break;

    float strand = float(i);
    float phase = strand * 1.7 * uSpread;
    float frequency = (2.0 + strand * 0.35) * uWaviness;
    float velocity = 1.4 + strand * 1.2;
    float time = uTime * uSpeed;
    float wave = sin(uv.x * frequency + time * velocity + phase) * 0.60
               + sin(uv.x * frequency * 1.1 - time * velocity * 0.7 + phase * 1.7) * 0.40;
    float amplitude = (0.1 + 0.02 * energy) * envelope * uAmplitude;
    float distanceToStrand = abs(uv.y - wave * amplitude);
    float thickness = (0.001 + 0.05 * energy) * (0.35 + envelope) * uThickness;
    float light = thickness / (distanceToStrand + thickness * 0.45);
    light *= light;

    float hue = strand / float(uStrandCount) + uv.x * 0.30 + uTime * 0.04 + uHueShift;
    col += samplePalette(hue) * light * envelope;
  }

  col *= 0.45 + 0.7 * energy;
  col = 1.0 - exp(-col * uGlow);
  float gray = dot(col, vec3(0.2126, 0.7152, 0.0722));
  col = max(mix(vec3(gray), col, uSaturation), 0.0);
  float alpha = clamp(max(max(col.r, col.g), col.b), 0.0, 1.0) * uOpacity;
  fragColor = vec4(col * uOpacity, alpha);
}
`;

  const canvases = Array.from(document.querySelectorAll?.("[data-strands]") ?? []);
  const renderers = canvases.map((canvas) => {
    const gl = canvas.getContext?.("webgl2", {
      alpha: true,
      antialias: true,
      premultipliedAlpha: true,
    });
    if (!gl) return null;

  function compile(type, source) {
    const shader = gl.createShader(type);
    gl.shaderSource(shader, source);
    gl.compileShader(shader);
    if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) throw new Error(gl.getShaderInfoLog(shader));
    return shader;
  }

  const program = gl.createProgram();
  gl.attachShader(program, compile(gl.VERTEX_SHADER, VERTEX_SHADER));
  gl.attachShader(program, compile(gl.FRAGMENT_SHADER, FRAGMENT_SHADER));
  gl.linkProgram(program);
  if (!gl.getProgramParameter(program, gl.LINK_STATUS)) throw new Error(gl.getProgramInfoLog(program));

  const buffer = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
  gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1, -1, 3, -1, -1, 3]), gl.STATIC_DRAW);
  const position = gl.getAttribLocation(program, "position");
  gl.enableVertexAttribArray(position);
  gl.vertexAttribPointer(position, 2, gl.FLOAT, false, 0, 0);

  const uniform = (name) => gl.getUniformLocation(program, name);
  const uniforms = {
    time: uniform("uTime"),
    resolution: uniform("uResolution"),
    colors: uniform("uColors[0]"),
    colorCount: uniform("uColorCount"),
    strandCount: uniform("uStrandCount"),
    speed: uniform("uSpeed"),
    amplitude: uniform("uAmplitude"),
    waviness: uniform("uWaviness"),
    thickness: uniform("uThickness"),
    glow: uniform("uGlow"),
    taper: uniform("uTaper"),
    spread: uniform("uSpread"),
    hueShift: uniform("uHueShift"),
    intensity: uniform("uIntensity"),
    opacity: uniform("uOpacity"),
    scale: uniform("uScale"),
    saturation: uniform("uSaturation"),
  };
  const reducedMotion = window.matchMedia?.("(prefers-reduced-motion: reduce)") ?? { matches: false };
  const styles = window.getComputedStyle(canvas);
  const palette = ["--turbo-strands-orange", "--turbo-strands-violet", "--turbo-strands-cyan"]
    .flatMap((name) => {
      const value = styles.getPropertyValue(name).trim().replace("#", "");
      const color = Number.parseInt(value, 16);
      return value.length === 6 && Number.isFinite(color)
        ? [((color >> 16) & 255) / 255, ((color >> 8) & 255) / 255, (color & 255) / 255]
        : [1, 1, 1];
    });
  let count = 0;
  let frame = 0;
  let sized = false;

  gl.clearColor(0, 0, 0, 0);
  gl.enable(gl.BLEND);
  gl.blendFunc(gl.ONE, gl.ONE_MINUS_SRC_ALPHA);
  gl.useProgram(program);
  gl.uniform3fv(uniforms.colors, new Float32Array(palette));
  gl.uniform1i(uniforms.colorCount, MAX_COLORS);
  gl.uniform1f(uniforms.speed, 0.5);
  gl.uniform1f(uniforms.amplitude, 1);
  gl.uniform1f(uniforms.waviness, 1);
  gl.uniform1f(uniforms.thickness, 0.7);
  gl.uniform1f(uniforms.glow, 2.6);
  gl.uniform1f(uniforms.taper, 3);
  gl.uniform1f(uniforms.spread, 1);
  gl.uniform1f(uniforms.hueShift, 0);
  gl.uniform1f(uniforms.intensity, 0.4);
  gl.uniform1f(uniforms.opacity, 1);
  gl.uniform1f(uniforms.scale, 1.875);
  gl.uniform1f(uniforms.saturation, 2);

  function draw(time = 0) {
    if (!sized) return;
    gl.clear(gl.COLOR_BUFFER_BIT);
    gl.uniform1f(uniforms.time, time * 0.001);
    gl.uniform1i(uniforms.strandCount, count);
    gl.drawArrays(gl.TRIANGLES, 0, 3);
  }

  function animate(time) {
    draw(time);
    frame = window.requestAnimationFrame(animate);
  }

  function restart() {
    if (frame) window.cancelAnimationFrame(frame);
    frame = 0;
    if (!count || reducedMotion.matches || !sized) draw();
    else frame = window.requestAnimationFrame(animate);
  }

  function resize() {
    const bounds = canvas.getBoundingClientRect();
    sized = Boolean(bounds.width && bounds.height);
    if (!sized) {
      restart();
      return;
    }
    const dpr = Math.min(window.devicePixelRatio || 1, 2);
    canvas.width = Math.round(bounds.width * dpr);
    canvas.height = Math.round(bounds.height * dpr);
    gl.viewport(0, 0, canvas.width, canvas.height);
    gl.uniform2f(uniforms.resolution, canvas.width, canvas.height);
    restart();
  }

  function setCount(value) {
    const next = Math.min(MAX_STRANDS, Math.max(0, Math.round(Number(value) || 0)));
    if (next === count) return;
    count = next;
    restart();
  }

  if (window.ResizeObserver) new window.ResizeObserver(resize).observe(canvas);
  else window.addEventListener("resize", resize, { passive: true });
  reducedMotion.addEventListener?.("change", restart);
  resize();
  setCount(canvas.dataset.count);
  return { setCount };
  }).filter(Boolean);

  if (!renderers.length) return;
  window.TurboStrands = {
    setCount(value) {
      renderers.forEach((renderer) => renderer.setCount(value));
    },
  };
})();
