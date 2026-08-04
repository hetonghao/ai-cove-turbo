(() => {
  "use strict";

  const TABS = ["live", "statistics", "config"];
  const ACTION_TO_STATE = {
    "toggle-compression": "compression",
    "toggle-websocket": "websocket",
    "toggle-autostart": "autostart",
    "restart-codex": "restart",
  };
  const BOOLEAN_STATES = new Set(["compression", "websocket", "autostart"]);
  const LIVE_REQUEST_LIMIT = 100;
  const LIVE_TEMPLATES = [
    { status: 200, path: "/v1/responses", raw: 186_420, sent: 82_110, transport: "WS", fallback: false },
    { status: 200, path: "/v1/responses", raw: 94_280, sent: 51_360, transport: "HTTP", fallback: false },
    { status: 200, path: "/v1/responses", raw: 238_940, sent: 109_420, transport: "WS", fallback: false },
    { status: 200, path: "/v1/responses", raw: 72_810, sent: 70_990, transport: "HTTP", fallback: true },
    { status: 201, path: "/v1/files", raw: 128_610, sent: 67_240, transport: "HTTP", fallback: false },
    { status: 200, path: "/v1/responses", raw: 310_480, sent: 142_730, transport: "WS", fallback: false },
    { status: 429, path: "/v1/responses", raw: 18_420, sent: 17_960, transport: "HTTP", fallback: true },
    { status: 200, path: "/v1/responses", raw: 156_870, sent: 71_240, transport: "WS", fallback: false },
    { status: 200, path: "/v1/responses", raw: 204_110, sent: 92_680, transport: "HTTP", fallback: false },
  ];
  const STAT_WINDOWS = [
    { label: "现在", age: 0.2, transport: "WS", result: "success", requests: 2, raw: 184_000, sent: 74_000 },
    { label: "−1m", age: 0.8, transport: "HTTP", result: "success", requests: 1, raw: 96_000, sent: 51_000 },
    { label: "−3m", age: 3, transport: "WS", result: "success", requests: 3, raw: 238_000, sent: 106_000 },
    { label: "−8m", age: 8, transport: "HTTP", result: "fallback", requests: 2, raw: 121_000, sent: 116_000 },
    { label: "−18m", age: 18, transport: "HTTP", result: "success", requests: 3, raw: 212_000, sent: 104_000 },
    { label: "−42m", age: 42, transport: "WS", result: "fallback", requests: 1, raw: 84_000, sent: 81_000 },
    { label: "−1h", age: 59, transport: "WS", result: "success", requests: 4, raw: 332_000, sent: 164_000 },
    { label: "−3h", age: 180, transport: "HTTP", result: "success", requests: 3, raw: 246_000, sent: 119_000 },
    { label: "−6h", age: 360, transport: "WS", result: "success", requests: 3, raw: 281_000, sent: 129_000 },
    { label: "−12h", age: 720, transport: "HTTP", result: "fallback", requests: 2, raw: 152_000, sent: 143_000 },
    { label: "−18h", age: 1_080, transport: "WS", result: "success", requests: 4, raw: 365_000, sent: 171_000 },
    { label: "−23h", age: 1_380, transport: "HTTP", result: "success", requests: 2, raw: 178_000, sent: 82_000 },
  ];
  const ROLLING_WINDOWS = [
    { label: "近 1 分钟", minutes: 1 },
    { label: "近 10 分钟", minutes: 10 },
    { label: "近 1 小时", minutes: 60 },
    { label: "近 1 天", minutes: 1_440 },
  ];
  const state = {
    tab: "live",
    serviceHealthy: true,
    configManaged: true,
    compression: true,
    compressionVerified: true,
    websocket: true,
    websocketVerified: true,
    websocketZstdVerified: true,
    autostart: true,
    restartRequired: false,
    restartRequested: false,
    actionFeedback: "所有状态均为本地原型演示",
    actionTone: "neutral",
    streamPaused: false,
  };
  const liveRequests = [];
  const numberFormatter = new Intl.NumberFormat("zh-CN");
  let liveTemplateIndex = 0;

  function readTab() {
    const params = new URL(window.location.href).searchParams;
    const requestedTab = params.get("tab");
    if (TABS.includes(requestedTab)) return requestedTab;
    if (requestedTab === "runtime") return "live";
    if (requestedTab === "stats") return "statistics";
    const variant = params.get("variant")?.toUpperCase();
    if (variant === "B") return "config";
    return "live";
  }

  function updateUrl() {
    const url = new URL(window.location.href);
    url.searchParams.delete("variant");
    url.searchParams.set("tab", state.tab);
    window.history.replaceState({}, "", url);
  }

  function formatBytes(bytes) {
    if (bytes >= 1_000_000) return `${(bytes / 1_000_000).toFixed(2)} MB`;
    if (bytes >= 1_000) {
      const kilobytes = bytes / 1_000;
      return `${kilobytes.toFixed(Number.isInteger(kilobytes) ? 0 : 1)} KB`;
    }
    return `${numberFormatter.format(bytes)} B`;
  }

  function formatRate(raw, sent) {
    if (!raw) return "—";
    return `${Math.max(0, (1 - sent / raw) * 100).toFixed(1)}%`;
  }

  function formatClock(date) {
    return [date.getHours(), date.getMinutes(), date.getSeconds()].map((part) => String(part).padStart(2, "0")).join(":");
  }

  function summarize(items) {
    return items.reduce((sum, item) => ({ requests: sum.requests + item.requests, raw: sum.raw + item.raw, sent: sum.sent + item.sent }), { requests: 0, raw: 0, sent: 0 });
  }

  function activationSummary() {
    if (state.restartRequested) return "重启请求已发送，等待 Codex 重新连接";
    if (state.restartRequired) return "需要重启 Codex 才会生效";
    if (!state.compression && state.websocketVerified && state.websocketZstdVerified) return "HTTP zstd 已关闭，WebSocket 已验证";
    if (state.compressionVerified && state.websocketVerified && state.websocketZstdVerified) return "HTTP / WebSocket 均已加速";
    return "通道可用，等待真实请求验证";
  }

  function statusFor(key) {
    const statuses = {
      "service-prerequisite": state.serviceHealthy ? "verified" : "waiting",
      "config-prerequisite": state.configManaged ? "verified" : "waiting",
      "restart-prerequisite": state.restartRequested ? "waiting" : state.restartRequired ? "required" : "verified",
      "http-status": state.serviceHealthy ? "verified" : "waiting",
      "http-zstd-status": !state.compression ? "disabled" : state.compressionVerified ? "verified" : "waiting",
      "websocket-handshake-status": !state.websocket ? "disabled" : state.websocketVerified ? "verified" : "waiting",
      "websocket-zstd-status": !state.websocket ? "disabled" : state.websocketZstdVerified ? "verified" : "waiting",
      "service-runtime": state.serviceHealthy ? "verified" : "waiting",
      "config-runtime": state.configManaged ? "verified" : "waiting",
      "restart-runtime": state.restartRequested ? "waiting" : state.restartRequired ? "required" : "verified",
      "http-zstd-runtime": !state.compression ? "disabled" : state.compressionVerified ? "verified" : "waiting",
      "websocket-runtime": !state.websocket ? "disabled" : state.websocketVerified ? "verified" : "waiting",
      "websocket-zstd-runtime": !state.websocket ? "disabled" : state.websocketZstdVerified ? "verified" : "waiting",
    };
    return statuses[key];
  }

  function formatState(key) {
    const values = {
      "activation-summary": activationSummary(),
      "service-prerequisite": state.serviceHealthy ? "正常" : "等待服务",
      "config-prerequisite": state.configManaged ? "已接管" : "等待接管",
      "restart-prerequisite": state.restartRequested ? "已请求" : state.restartRequired ? "待重启" : "已完成",
      "http-status": state.serviceHealthy ? "通道可用" : "通道不可用",
      "http-zstd-status": !state.compression ? "zstd 已关闭" : state.compressionVerified ? "zstd 已验证" : "zstd 等待验证",
      "websocket-handshake-status": !state.websocket ? "握手已关闭" : state.websocketVerified ? "握手已验证" : "握手等待验证",
      "websocket-zstd-status": !state.websocket ? "zstd 已关闭" : state.websocketZstdVerified ? "zstd 已验证" : "zstd 等待验证",
      "action-feedback": state.actionFeedback,
      compression: state.compression ? "开" : "关",
      websocket: state.websocket ? "开" : "关",
      autostart: state.autostart ? "开" : "关",
      restart: state.restartRequested ? "已请求重启" : state.restartRequired ? "需要重启 Codex" : "重启 Codex",
      "service-runtime": state.serviceHealthy ? "正常" : "离线",
      "config-runtime": state.configManaged ? "已接管" : "等待中",
      "restart-runtime": state.restartRequested ? "已请求" : state.restartRequired ? "待重启" : "就绪",
      "http-zstd-runtime": !state.compression ? "已关闭" : state.compressionVerified ? "已验证" : "待验证",
      "websocket-runtime": !state.websocket ? "已关闭" : state.websocketVerified ? "已验证" : "待验证",
      "websocket-zstd-runtime": !state.websocket ? "已关闭" : state.websocketZstdVerified ? "已验证" : "待验证",
    };
    return String(values[key] ?? "");
  }

  function renderState() {
    document.body.dataset.restartRequired = String(state.restartRequired);
    document.querySelectorAll("[data-state]").forEach((target) => {
      const key = target.dataset.state;
      target.textContent = formatState(key);
      const status = statusFor(key);
      if (status) target.dataset.status = status;
    });

    document.querySelectorAll("[data-action]").forEach((control) => {
      const key = ACTION_TO_STATE[control.dataset.action];
      if (BOOLEAN_STATES.has(key)) {
        control.dataset.enabled = String(state[key]);
        control.setAttribute("aria-pressed", String(state[key]));
      } else if (key === "restart") {
        control.dataset.required = String(state.restartRequired);
        control.dataset.status = state.restartRequested ? "requested" : state.restartRequired ? "required" : "idle";
      }
    });

    const feedback = document.querySelector(".b-action-feedback");
    if (feedback) feedback.dataset.tone = state.actionTone;
  }

  function renderTab(options = {}) {
    document.body.dataset.activeTab = state.tab;
    document.querySelectorAll("[data-tab]").forEach((tab) => {
      const active = tab.dataset.tab === state.tab;
      tab.setAttribute("aria-selected", String(active));
      tab.tabIndex = active ? 0 : -1;
      if (active && options.focus) tab.focus();
    });
    document.querySelectorAll("[data-panel]").forEach((panel) => {
      panel.hidden = panel.dataset.panel !== state.tab;
    });
  }

  function selectTab(tab, options = {}) {
    if (!TABS.includes(tab)) return;
    state.tab = tab;
    renderTab(options);
    if (options.updateUrl !== false) updateUrl();
  }

  function renderLiveStream() {
    const body = document.querySelector("[data-request-stream]");
    if (!body) return;
    body.innerHTML = liveRequests.map((request) => {
      const statusTone = request.status < 400 ? "success" : "error";
      const transport = request.fallback ? `${request.transport} · 回退` : `${request.transport} · zstd`;
      return `<tr><td>${request.time}</td><td><span class="c-request-status c-request-status--${statusTone}">${request.status}</span></td><td><code>${request.path}</code></td><td><strong>${formatBytes(request.raw)}</strong><span aria-hidden="true">→</span><strong>${formatBytes(request.sent)}</strong></td><td><span class="c-transport${request.fallback ? " c-transport--fallback" : ""}">${transport}</span></td><td>${formatRate(request.raw, request.sent)}</td></tr>`;
    }).join("");

    const empty = document.querySelector("[data-stream-empty]");
    if (empty) empty.hidden = liveRequests.length > 0;
    document.querySelectorAll("[data-live-count]").forEach((target) => {
      target.textContent = numberFormatter.format(liveRequests.length);
    });
    const streamState = document.querySelector("[data-live-stream-state]");
    if (streamState) {
      streamState.classList.toggle("is-paused", state.streamPaused);
      streamState.lastChild.textContent = state.streamPaused ? "已暂停" : "实时更新";
    }
    const toggle = document.querySelector('[data-action="toggle-stream"]');
    if (toggle) toggle.setAttribute("aria-pressed", String(state.streamPaused));
    const label = document.querySelector("[data-live-action-label]");
    if (label) label.textContent = state.streamPaused ? "继续" : "暂停";
    const windowElement = document.querySelector(".c-terminal__window");
    if (windowElement) windowElement.scrollTop = windowElement.scrollHeight;
  }

  function addLiveRequest(template = LIVE_TEMPLATES[liveTemplateIndex++ % LIVE_TEMPLATES.length], date = new Date()) {
    liveRequests.push({ ...template, time: formatClock(date) });
    if (liveRequests.length > LIVE_REQUEST_LIMIT) liveRequests.shift();
    renderLiveStream();
  }

  function renderStatistics() {
    const range = Number(document.querySelector('[data-filter="range"]')?.value ?? 1_440);
    const transport = document.querySelector('[data-filter="transport"]')?.value ?? "all";
    const result = document.querySelector('[data-filter="result"]')?.value ?? "all";
    const matching = STAT_WINDOWS.filter((item) => (transport === "all" || item.transport === transport) && (result === "all" || item.result === result));
    const filtered = matching.filter((item) => item.age <= range);
    const totals = summarize(filtered);
    const values = {
      requests: numberFormatter.format(totals.requests),
      "raw-bytes": formatBytes(totals.raw),
      "sent-bytes": formatBytes(totals.sent),
      "saved-bytes": formatBytes(Math.max(0, totals.raw - totals.sent)),
      "savings-rate": formatRate(totals.raw, totals.sent),
    };
    document.querySelectorAll("[data-stat]").forEach((target) => {
      target.textContent = values[target.dataset.stat] ?? "";
    });

    const rangeLabels = { 1: "最近 1 分钟", 10: "最近 10 分钟", 60: "最近 1 小时", 1440: "最近 1 天" };
    const transportLabel = transport === "all" ? "全部方式" : transport === "WS" ? "WebSocket" : "HTTP";
    const resultLabel = result === "all" ? "全部结果" : result === "success" ? "成功" : "回退";
    const summary = document.querySelector("[data-stats-summary]");
    if (summary) summary.textContent = `${rangeLabels[range]} / ${transportLabel} / ${resultLabel}`;

    const bars = document.querySelector("[data-stat-bars]");
    if (bars) {
      const maxRaw = Math.max(...filtered.map((item) => item.raw), 1);
      bars.innerHTML = filtered.map((item) => `<span class="c-bar-slot"><i class="c-bar" style="--bar: ${Math.round(18 + item.raw / maxRaw * 82)}%; --sent-share: ${Math.round(item.sent / item.raw * 100)}%"></i><small>${item.label}</small></span>`).join("");
    }
    const windows = document.querySelector("[data-stat-windows]");
    if (windows) {
      windows.innerHTML = ROLLING_WINDOWS.map((window) => {
        const windowTotals = summarize(matching.filter((item) => item.age <= window.minutes));
        return `<article class="c-window-card"><header><span>${window.label}</span><strong>${numberFormatter.format(windowTotals.requests)} 个请求</strong></header><div><p><span>请求数</span><strong>${numberFormatter.format(windowTotals.requests)}</strong></p><p><span>原始 → 发送</span><strong>${formatBytes(windowTotals.raw)} → ${formatBytes(windowTotals.sent)}</strong></p><p><span>节省率</span><strong>${formatRate(windowTotals.raw, windowTotals.sent)}</strong></p></div></article>`;
      }).join("");
    }
    const empty = document.querySelector("[data-stat-empty]");
    if (empty) empty.hidden = filtered.length > 0;
  }

  function handleAction(action) {
    if (action === "toggle-stream") {
      state.streamPaused = !state.streamPaused;
      renderLiveStream();
      return;
    }
    if (action === "clear-stream") {
      liveRequests.length = 0;
      renderLiveStream();
      return;
    }

    const key = ACTION_TO_STATE[action];
    if (!key) return;
    if (key === "restart") {
      state.restartRequested = true;
      state.actionFeedback = "已发送重启请求（原型演示）；等待 Codex 重新连接";
      state.actionTone = "notice";
    } else {
      state[key] = !state[key];
      state.actionTone = "success";
      if (key === "compression") {
        state.compressionVerified = false;
        state.actionFeedback = state.compression ? "已开启 HTTP 压缩；等待下一次真实请求验证 zstd" : "已关闭 HTTP 压缩；WebSocket 与 WS zstd 状态不变";
      } else if (key === "websocket") {
        state.websocketVerified = false;
        state.websocketZstdVerified = false;
        state.restartRequired = true;
        state.restartRequested = false;
        state.actionFeedback = state.websocket ? "已开启 WebSocket；重启 Codex 后等待握手与 zstd 分别验证" : "已关闭 WebSocket；HTTP 通道与 HTTP zstd 保持独立";
      } else {
        state.actionFeedback = state.autostart ? "已开启开机自启动；Turbo 将在登录后于后台运行" : "已关闭开机自启动；当前 Turbo 仍在后台运行";
      }
    }
    renderState();
  }

  function handleTabKeydown(event, currentTab) {
    const currentIndex = TABS.indexOf(currentTab.dataset.tab);
    const keys = { ArrowRight: 1, ArrowLeft: -1, Home: -currentIndex, End: TABS.length - 1 - currentIndex };
    if (!Object.hasOwn(keys, event.key)) return;
    event.preventDefault();
    selectTab(TABS[(currentIndex + keys[event.key] + TABS.length) % TABS.length], { focus: true });
  }

  function bindDotField() {
    const surface = document.querySelector(".turbo-panels");
    const canvas = surface?.querySelector("[data-dot-field]");
    const context = canvas?.getContext("2d");
    if (!surface || !canvas || !context) return;

    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
    const finePointer = window.matchMedia("(pointer: fine)");
    const BULGE_STRENGTH = 130;
    const dots = [];
    const pointer = { x: 0, y: 0, lastX: 0, lastY: 0, lastTime: 0, engagement: 0, inside: false };
    let width = 0;
    let height = 0;
    let frame = 0;

    function draw() {
      context.clearRect(0, 0, width, height);
      const color = context.createLinearGradient(0, 0, width, height);
      color.addColorStop(0, "rgba(112, 216, 238, 0.26)");
      color.addColorStop(0.52, "rgba(114, 217, 155, 0.44)");
      color.addColorStop(1, "rgba(112, 216, 238, 0.18)");
      context.fillStyle = color;
      for (const dot of dots) {
        context.beginPath();
        context.arc(dot.x + dot.dx, dot.y + dot.dy, 1.2, 0, Math.PI * 2);
        context.fill();
      }

    }

    function resize() {
      const bounds = surface.getBoundingClientRect();
      if (!bounds.width || !bounds.height) return;
      width = bounds.width;
      height = bounds.height;
      const dpr = Math.min(window.devicePixelRatio || 1, 2);
      canvas.width = Math.round(width * dpr);
      canvas.height = Math.round(height * dpr);
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
      context.setTransform(dpr, 0, 0, dpr, 0, 0);
      dots.length = 0;
      for (let y = 9; y < height; y += 18) {
        for (let x = 9; x < width; x += 18) dots.push({ x, y, dx: 0, dy: 0 });
      }
      draw();
      canvas.dataset.dotState = "rest";
    }

    function animate() {
      frame = 0;
      let moving = false;
      pointer.engagement *= pointer.inside ? 0.9 : 0.78;
      for (const dot of dots) {
        const x = dot.x - pointer.x;
        const y = dot.y - pointer.y;
        const distance = Math.hypot(x, y) || 1;
        const influence = pointer.inside && distance < 180 ? (1 - distance / 180) ** 2 * pointer.engagement : 0;
        const targetX = x / distance * BULGE_STRENGTH * influence;
        const targetY = y / distance * BULGE_STRENGTH * influence;
        dot.dx += (targetX - dot.dx) * 0.18;
        dot.dy += (targetY - dot.dy) * 0.18;
        if (Math.abs(targetX - dot.dx) + Math.abs(targetY - dot.dy) > 0.04 || Math.abs(dot.dx) + Math.abs(dot.dy) > 0.05) moving = true;
      }
      draw();
      if (moving || pointer.engagement > 0.02) {
        canvas.dataset.dotState = pointer.inside && pointer.engagement > 0.08 ? "active" : "settling";
        frame = window.requestAnimationFrame(animate);
      } else {
        pointer.engagement = 0;
        canvas.dataset.dotState = "rest";
      }
    }

    function start() {
      if (!frame) frame = window.requestAnimationFrame(animate);
    }

    surface.addEventListener("pointermove", (event) => {
      if (reduceMotion.matches || !finePointer.matches || width <= 768) return;
      const bounds = surface.getBoundingClientRect();
      const now = performance.now();
      pointer.x = event.clientX - bounds.left;
      pointer.y = event.clientY - bounds.top;
      const elapsed = Math.max(16, now - pointer.lastTime);
      const speed = Math.hypot(pointer.x - pointer.lastX, pointer.y - pointer.lastY) / elapsed;
      pointer.engagement = Math.max(pointer.engagement, Math.min(1, speed / 0.8));
      pointer.lastX = pointer.x;
      pointer.lastY = pointer.y;
      pointer.lastTime = now;
      pointer.inside = true;
      start();
    });
    surface.addEventListener("pointerleave", () => {
      pointer.inside = false;
      start();
    });

    if (window.ResizeObserver) new ResizeObserver(resize).observe(surface);
    else window.addEventListener("resize", resize, { passive: true });
    reduceMotion.addEventListener?.("change", () => {
      pointer.inside = false;
      pointer.engagement = 0;
      for (const dot of dots) dot.dx = dot.dy = 0;
      draw();
      canvas.dataset.dotState = "rest";
    });
    resize();
  }

  function init() {
    state.tab = readTab();
    LIVE_TEMPLATES.slice(0, 8).forEach((request, index) => addLiveRequest(request, new Date(Date.now() - (7 - index) * 1_300)));
    liveTemplateIndex = 8;
    document.addEventListener("click", (event) => {
      const action = event.target.closest?.("[data-action]");
      if (action) handleAction(action.dataset.action);
      const tab = event.target.closest?.("[data-tab]");
      if (tab) selectTab(tab.dataset.tab);
    });
    document.addEventListener("keydown", (event) => {
      const tab = event.target.closest?.("[data-tab]");
      if (tab) handleTabKeydown(event, tab);
    });
    document.addEventListener("change", (event) => {
      if (event.target.matches?.("[data-filter]")) renderStatistics();
    });
    window.addEventListener("popstate", () => {
      state.tab = readTab();
      renderTab();
    });
    bindDotField();
    renderTab();
    renderState();
    renderLiveStream();
    renderStatistics();
    updateUrl();
    window.setInterval(() => {
      if (state.tab === "live" && !state.streamPaused) addLiveRequest();
    }, 3_500);
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init, { once: true });
  } else {
    init();
  }
})();
