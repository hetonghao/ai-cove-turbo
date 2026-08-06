(() => {
  "use strict";

  const TABS = ["live", "statistics", "config"];
  const RANGE_LABELS = { 1: "最近 1 分钟", 10: "最近 10 分钟", 60: "最近 1 小时", 1440: "最近 1 天" };
  const ROLLING_WINDOWS = [1, 10, 60, 1440];
  const invoke = window.__TAURI__?.core?.invoke;
  const telemetry = window.TurboTelemetry;
  const numberFormatter = new Intl.NumberFormat("zh-CN");
  const motion = { pageMs: 180, revealMs: 140, shiftPx: 10, blurPx: 3, easing: "cubic-bezier(0.16, 1, 0.3, 1)" };
  const $ = (selector) => document.querySelector?.(selector) ?? null;
  const all = (selector) => document.querySelectorAll?.(selector) ?? [];

  const desktopStatus = {
    serviceHealthy: false,
    endpoint: "—",
    configState: "starting",
    configMessage: "正在读取 Turbo 状态",
    technicalDetail: "",
    provider: "—",
    upstream: "—",
    aiCoveUpstream: true,
    aiCoveUpstreamFixAvailable: false,
    compressionEnabled: true,
    compressionVerified: false,
    websocketEnabled: true,
    websocketVerified: false,
    websocketZstdVerified: false,
    websocketState: "waiting",
    websocketHandshakes: 0,
    websocketMessages: 0,
    websocketRawBytes: 0,
    websocketSentBytes: 0,
    httpFallbacks: 0,
    autostartEnabled: true,
    dockVisible: false,
    dockControlAvailable: true,
    restartRequired: false,
    desktopRestarted: false,
    requests: 0,
    rawBytes: 0,
    sentBytes: 0,
    compressionRatio: 0,
    recentRequests: [],
    trafficWindows: [],
    updateState: "idle",
    updateMessage: "尚未检查更新",
    updateProgress: 0,
  };

  function buildPreviewTelemetry() {
    const now = Date.now();
    const samples = [
      { id: 1, ageSeconds: 3, status: 200, path: "/v1/responses", rawBytes: 186_420, sentBytes: 82_110, transport: "WS", result: "success" },
      { id: 2, ageSeconds: 11, status: 200, path: "/v1/responses", rawBytes: 94_280, sentBytes: 51_360, transport: "HTTP", result: "success" },
      { id: 3, ageSeconds: 48, status: 201, path: "/v1/files", rawBytes: 128_610, sentBytes: 67_240, transport: "HTTP", result: "success" },
      { id: 4, ageSeconds: 210, status: 200, path: "/v1/responses", rawBytes: 121_000, sentBytes: 116_000, transport: "HTTP", result: "fallback" },
      { id: 5, ageSeconds: 1_080, status: 200, path: "/v1/responses", rawBytes: 212_000, sentBytes: 104_000, transport: "WS", result: "success" },
      { id: 6, ageSeconds: 10_800, status: 200, path: "/v1/responses", rawBytes: 246_000, sentBytes: 119_000, transport: "HTTP", result: "success" },
      { id: 7, ageSeconds: 43_200, status: 200, path: "/v1/responses", rawBytes: 152_000, sentBytes: 143_000, transport: "HTTP", result: "fallback" },
      { id: 8, ageSeconds: 82_800, status: 200, path: "/v1/responses", rawBytes: 178_000, sentBytes: 82_000, transport: "WS", result: "success" },
    ].map((sample) => ({ ...sample, timestampMs: now - sample.ageSeconds * 1_000 }));
    const specs = [[1, 10, 6], [10, 60, 10], [60, 300, 12], [1440, 3_600, 24]];
    const trafficWindows = specs.map(([minutes, bucketSeconds, bucketCount]) => {
      const bucketMs = bucketSeconds * 1_000;
      const currentPeriodStartMs = now - now % bucketMs;
      const rangeStart = currentPeriodStartMs - (bucketCount - 1) * bucketMs;
      const buckets = Array.from({ length: bucketCount }, (_, index) => ({
        startMs: rangeStart + index * bucketMs,
        endMs: rangeStart + (index + 1) * bucketMs,
        series: [],
      }));
      samples.forEach((sample) => {
        const index = Math.floor((sample.timestampMs - rangeStart) / bucketMs);
        if (index < 0 || index >= buckets.length) return;
        const key = `${sample.transport}:${sample.result}`;
        let series = buckets[index].series.find((item) => item.key === key);
        if (!series) {
          series = { key, transport: sample.transport, result: sample.result, requests: 0, rawBytes: 0, sentBytes: 0 };
          buckets[index].series.push(series);
        }
        series.requests += 1;
        series.rawBytes += sample.rawBytes;
        series.sentBytes += sample.sentBytes;
      });
      buckets.forEach((bucket) => bucket.series.forEach((series) => delete series.key));
      return { minutes, bucketSeconds, currentPeriodStartMs, buckets };
    });
    return { recentRequests: samples.slice(0, 5).reverse(), trafficWindows };
  }

  const previewTelemetry = buildPreviewTelemetry();
  const previewStatus = {
    ...desktopStatus,
    ...previewTelemetry,
    serviceHealthy: true,
    endpoint: "http://127.0.0.1:44175/v1",
    configState: "managed",
    configMessage: "Preview：配置已生效",
    provider: "ai-cove",
    upstream: "https://api.ai-cove.com/v1",
    compressionVerified: true,
    websocketVerified: true,
    websocketZstdVerified: true,
    websocketState: "connected",
    websocketHandshakes: 8,
    websocketMessages: 12,
    httpFallbacks: 2,
    requests: 24,
    rawBytes: 1_840_000,
    sentBytes: 1_060_000,
    compressionRatio: 42.4,
    updateMessage: "Preview：尚未检查更新",
  };
  let state = { ...(invoke ? desktopStatus : previewStatus), tab: "live", nonAiCoveConfirmed: false };
  let pendingAction = "";
  let refreshing = false;
  let streamPaused = false;
  let clearedThroughId = 0;
  let displayedRequests = [];
  let renderedBarMarkup = "";
  let activeChartIndex = 0;

  const actions = {
    "toggle-compression": ["set_compression", () => ({ enabled: !state.compressionEnabled })],
    "toggle-websocket": ["set_websocket", () => ({ enabled: !state.websocketEnabled })],
    "toggle-autostart": ["set_autostart", () => ({ enabled: !state.autostartEnabled })],
    "toggle-dock": ["set_dock_visible", () => ({ visible: !state.dockVisible })],
    "restart-codex": ["restart_codex"],
    "retry-takeover": ["retry_takeover"],
    "set-ai-cove-upstream": ["set_ai_cove_upstream"],
    "confirm-non-ai-cove": ["confirm_non_ai_cove"],
    "check-for-updates": ["check_for_updates"],
    "install-update": ["install_update"],
  };

  function readTab() {
    const requestedTab = new URL(window.location.href).searchParams.get("tab");
    if (TABS.includes(requestedTab)) return requestedTab;
    if (requestedTab === "runtime") return "live";
    if (requestedTab === "stats") return "statistics";
    return "live";
  }

  function updateUrl() {
    const url = new URL(window.location.href);
    url.searchParams.delete("variant");
    url.searchParams.set("tab", state.tab);
    window.history.replaceState({}, "", url);
  }

  function formatConfigState() {
    const labels = {
      active: "已生效",
      healthy: "已生效",
      managed: "已生效",
      starting: "检查中",
      warning: "等待确认",
      blocked: "已阻塞",
      conflict: "配置冲突",
      restored: "已恢复原配置",
      error: "发生错误",
      missing: "未找到配置",
    };
    return labels[String(state.configState).toLowerCase()] ?? state.configState ?? "未知";
  }

  function configReady() {
    return ["active", "healthy", "managed"].includes(String(state.configState).toLowerCase());
  }

  function formatVerification() {
    if (!state.compressionEnabled) return "已关闭";
    return state.compressionVerified ? "压缩已验证（zstd）" : "等待真实请求验证";
  }

  function formatWebsocketStatus() {
    if (!state.websocketEnabled) return "已关闭";
    const labels = {
      connected: "连接已验证",
      closed: "已验证 · 当前已关闭",
      failed: "连接失败 · 已回退 HTTP",
      conflict: "配置被外部修改",
      waiting: "等待首次连接验证",
    };
    return labels[String(state.websocketState).toLowerCase()] ?? "等待首次连接验证";
  }

  function formatUpdateState() {
    const labels = {
      idle: "尚未检查",
      checking: "检查中",
      current: "已是最新",
      available: "发现新版本",
      downloaded: "可安装",
      downloading: "下载中",
      installing: "安装中",
      ready: "安装完成",
      unconfigured: "未配置",
      error: "更新失败",
    };
    return labels[String(state.updateState).toLowerCase()] ?? state.updateState ?? "未知";
  }

  function activationSummary() {
    if (!state.serviceHealthy) return "本地通道尚未就绪";
    if (!configReady()) return "等待 Turbo 完成配置";
    if (state.restartRequired) return "需要重启 Codex 才会生效";
    const httpReady = !state.compressionEnabled || state.compressionVerified;
    const websocketReady = !state.websocketEnabled || (state.websocketVerified && state.websocketZstdVerified);
    if (httpReady && websocketReady) return "HTTP / WebSocket 均已生效";
    return "通道可用，等待真实请求验证";
  }

  function totalRequests() {
    return (Number(state.requests) || 0) + (Number(state.websocketMessages) || 0);
  }

  function formatState(key) {
    const configState = String(state.configState ?? "unknown").toUpperCase();
    const starting = configState === "STARTING";
    const observed = totalRequests() > 0 || Number(state.websocketHandshakes) > 0;
    const values = {
      "runtime-mode": invoke ? "DESKTOP" : "PREVIEW",
      "service-label": `${invoke ? "AI Cove" : "PREVIEW"} / ${starting ? "正在读取状态" : state.serviceHealthy ? "本地服务正常" : "本地服务异常"}`,
      "service-title": starting ? "正在读取状态" : state.serviceHealthy ? "已连接并生效" : "通道未就绪",
      endpoint: state.endpoint || "—",
      "config-state": formatConfigState(),
      "config-message": state.configMessage || "—",
      provider: state.provider || "—",
      upstream: state.upstream || "—",
      "compression-verified": formatVerification(),
      compression: state.compressionEnabled ? "开" : "关",
      websocket: state.websocketEnabled ? "开" : "关",
      "websocket-status": formatWebsocketStatus(),
      "websocket-detail": state.websocketEnabled ? "扩展由上游协商" : "未启用",
      "websocket-handshakes": numberFormatter.format(Number(state.websocketHandshakes) || 0),
      "http-fallbacks": numberFormatter.format(Number(state.httpFallbacks) || 0),
      autostart: state.autostartEnabled ? "开" : "关",
      dock: state.dockVisible ? "开" : "关",
      restart: pendingAction === "restart-codex" ? "正在重启…" : state.restartRequired ? "重启 Codex" : "重新启动 Codex",
      requests: numberFormatter.format(totalRequests()),
      "raw-bytes": telemetry.formatBytes((Number(state.rawBytes) || 0) + (Number(state.websocketRawBytes) || 0)),
      "sent-bytes": telemetry.formatBytes((Number(state.sentBytes) || 0) + (Number(state.websocketSentBytes) || 0)),
      ratio: telemetry.formatRate(
        (Number(state.rawBytes) || 0) + (Number(state.websocketRawBytes) || 0),
        (Number(state.sentBytes) || 0) + (Number(state.websocketSentBytes) || 0),
      ),
      "update-state": formatUpdateState(),
      "update-message": state.updateMessage || "—",
      "update-progress": `${Math.max(0, Math.min(100, Number(state.updateProgress) || 0))}%`,
      "service-runtime": starting ? "正在读取" : state.serviceHealthy ? "正常" : "离线",
      "config-runtime": starting ? "检查中" : configReady() ? "已生效" : formatConfigState(),
      "restart-runtime": state.restartRequired ? "需要重启" : observed || state.desktopRestarted ? "已生效" : "待确认",
      "http-zstd-runtime": !state.compressionEnabled ? "已关闭" : state.compressionVerified ? "已验证" : "待验证",
      "websocket-runtime": !state.websocketEnabled ? "已关闭" : state.websocketVerified ? "已验证" : String(state.websocketState).toLowerCase() === "failed" ? "连接失败" : "待验证",
      "websocket-zstd-runtime": !state.websocketEnabled ? "已关闭" : state.websocketZstdVerified ? "已验证" : "待验证",
      "service-prerequisite": starting ? "检查中" : state.serviceHealthy ? "正常" : "异常",
      "config-prerequisite": starting ? "检查中" : configReady() ? "已生效" : formatConfigState(),
      "restart-prerequisite": state.restartRequired ? "需要重启" : observed || state.desktopRestarted ? "已生效" : "待确认",
      "http-status": state.serviceHealthy ? "通道可用" : "通道不可用",
      "http-zstd-status": !state.compressionEnabled ? "已关闭" : state.compressionVerified ? "压缩已验证" : "等待验证",
      "websocket-handshake-status": !state.websocketEnabled ? "已关闭" : state.websocketVerified ? "连接已验证" : "等待连接",
      "websocket-zstd-status": !state.websocketEnabled ? "已关闭" : state.websocketZstdVerified ? "压缩已验证" : "等待验证",
      "activation-summary": activationSummary(),
      "observed-state": observed ? "OBSERVED / LIVE" : "OBSERVED / WAITING",
      "stream-state": starting ? "WAITING" : state.serviceHealthy ? (observed ? "ACTIVE" : "IDLE") : "OFFLINE",
    };
    return String(values[key] ?? "");
  }

  function statusFor(key) {
    if (key.startsWith("service")) return state.serviceHealthy ? "verified" : String(state.configState).toLowerCase() === "starting" ? "waiting" : "blocked";
    if (key.startsWith("config")) return configReady() ? "verified" : String(state.configState).toLowerCase() === "starting" ? "waiting" : "blocked";
    if (key.startsWith("restart")) return state.restartRequired ? "required" : totalRequests() > 0 || state.desktopRestarted ? "verified" : "waiting";
    if (key.startsWith("http-zstd")) return !state.compressionEnabled ? "disabled" : state.compressionVerified ? "verified" : "waiting";
    if (key === "http-status") return state.serviceHealthy ? "verified" : "blocked";
    if (key.startsWith("websocket-zstd")) return !state.websocketEnabled ? "disabled" : state.websocketZstdVerified ? "verified" : "waiting";
    if (key.startsWith("websocket")) return !state.websocketEnabled ? "disabled" : state.websocketVerified ? "verified" : String(state.websocketState).toLowerCase() === "failed" ? "blocked" : "waiting";
    return "";
  }

  function activationVerifiedCount() {
    return [
      statusFor("service-prerequisite") === "verified",
      statusFor("config-prerequisite") === "verified",
      statusFor("restart-prerequisite") === "verified",
      statusFor("http-status") === "verified" && statusFor("http-zstd-status") === "verified",
      statusFor("websocket-handshake-status") === "verified" && statusFor("websocket-zstd-status") === "verified",
    ].filter(Boolean).length;
  }

  function liveRecovery() {
    const configState = String(state.configState).toLowerCase();
    if (configState === "starting" && !state.technicalDetail) return null;
    if (!state.serviceHealthy) {
      return {
        title: "本地服务离线",
        message: "Turbo 无法读取本地通道，请前往配置页检查服务与上游。",
        action: "open-config",
        label: "查看配置",
        detail: state.technicalDetail,
      };
    }
    if (state.restartRequired) {
      return {
        title: "Codex 需要重启",
        message: "配置已写入，重启后会重新验证传输通道。",
        action: "restart-codex",
        label: "立即重启",
        detail: "",
      };
    }
    if (!configReady()) {
      const retryable = ["blocked", "conflict", "error"].includes(configState)
        && !state.aiCoveUpstreamFixAvailable;
      const messages = {
        blocked: "Turbo 尚未完成配置，请重试后再次验证。",
        conflict: "Codex 配置已被外部修改，请重新应用 Turbo 配置。",
        error: "配置没有成功写入，请重试并查看技术详情。",
        missing: "没有找到 Codex 配置，请先打开 Codex 完成初始化。",
        warning: "当前上游需要确认，请在配置页完成选择。",
      };
      return {
        title: configState === "conflict" ? "配置发生冲突" : "配置尚未生效",
        message: messages[configState] ?? "请前往配置页完成检查。",
        action: retryable ? "retry-takeover" : "open-config",
        label: retryable ? "重试配置" : "查看配置",
        detail: state.technicalDetail,
      };
    }
    if (state.websocketEnabled && String(state.websocketState).toLowerCase() === "failed") {
      return {
        title: "WebSocket 连接失败",
        message: "请求已自动回退到 HTTP，可在配置页检查 WebSocket 设置。",
        action: "open-config",
        label: "查看配置",
        detail: state.technicalDetail,
      };
    }
    return null;
  }

  function reduceMotion() {
    return Boolean(window.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches);
  }

  function cancelMotion(target) {
    target?.getAnimations?.().forEach((animation) => animation.cancel());
  }

  function playMotion(target, keyframes, duration) {
    if (!target?.animate || reduceMotion()) return;
    cancelMotion(target);
    target.animate(keyframes, { duration, easing: motion.easing });
  }

  function setConditionalVisibility(target, visible) {
    const wasHidden = target.hidden;
    if (!visible) cancelMotion(target);
    target.hidden = !visible;
    if (visible && wasHidden) playMotion(target, [{ opacity: 0 }, { opacity: 1 }], motion.revealMs);
  }

  function renderLiveRecovery() {
    const container = $("[data-live-recovery]");
    if (!container) return;
    const recovery = liveRecovery();
    setConditionalVisibility(container, Boolean(recovery));
    if (!recovery) return;
    const title = $("[data-live-recovery-title]");
    const message = $("[data-live-recovery-message]");
    const action = $("[data-live-recovery-action]");
    const details = $("[data-live-recovery-details]");
    const detail = $("[data-live-recovery-detail]");
    if (title) title.textContent = recovery.title;
    if (message) message.textContent = recovery.message;
    if (action) {
      action.dataset.action = recovery.action;
      action.textContent = recovery.label;
    }
    if (details) details.hidden = !recovery.detail;
    if (detail) detail.textContent = recovery.detail || "";
  }

  function renderVisibility() {
    const configState = String(state.configState).toLowerCase();
    const visible = {
      dock: Boolean(state.dockControlAvailable),
      "non-ai-cove": !state.aiCoveUpstream && !state.aiCoveUpstreamFixAvailable,
      "ai-cove-upstream": Boolean(state.aiCoveUpstreamFixAvailable),
      "confirm-non-ai-cove": configState === "warning" && !state.nonAiCoveConfirmed,
      retry: !state.aiCoveUpstreamFixAvailable && (configState === "blocked" || (configState === "conflict" && !state.serviceHealthy)),
      "install-update": ["available", "downloaded"].includes(String(state.updateState).toLowerCase()),
      "update-progress": ["downloading", "installing"].includes(String(state.updateState).toLowerCase()),
    };
    all("[data-visible]").forEach((target) => {
      setConditionalVisibility(target, Boolean(visible[target.dataset.visible]));
    });
  }

  function renderControls() {
    const pressed = {
      "toggle-compression": state.compressionEnabled,
      "toggle-websocket": state.websocketEnabled,
      "toggle-autostart": state.autostartEnabled,
      "toggle-dock": state.dockVisible,
    };
    all("[data-action]").forEach((control) => {
      const action = control.dataset.action;
      const managed = Object.hasOwn(actions, action);
      if (managed) {
        control.disabled = Boolean(pendingAction);
        control.dataset.status = pendingAction === action ? "pending" : "idle";
        control.setAttribute("aria-busy", String(pendingAction === action));
      }
      if (action === "restart-codex") control.dataset.required = String(Boolean(state.restartRequired));
      if (Object.hasOwn(pressed, action)) {
        control.dataset.enabled = String(pressed[action]);
        control.setAttribute("aria-pressed", String(pressed[action]));
      }
    });
  }

  function renderState() {
    document.body.dataset.serviceHealthy = String(Boolean(state.serviceHealthy));
    all("[data-state]").forEach((target) => {
      const key = target.dataset.state;
      target.textContent = formatState(key);
      const status = statusFor(key);
      if (status) target.dataset.status = status;
    });
    all("[data-state-progress]").forEach((target) => {
      target.style.setProperty("--progress", `${Math.max(0, Math.min(100, Number(state.updateProgress) || 0))}%`);
    });
    all('[role="progressbar"]').forEach((target) => {
      target.setAttribute("aria-valuenow", String(Math.max(0, Math.min(100, Number(state.updateProgress) || 0))));
    });
    const count = activationVerifiedCount();
    all("[data-strands]").forEach((strands) => {
      strands.dataset.count = String(count);
      strands.setAttribute("aria-label", `${count}/5 项已验证`);
    });
    window.TurboStrands?.setCount(count);
    renderVisibility();
    renderLiveRecovery();
    renderControls();
    renderLiveStream();
    renderStatistics();
  }

  function renderTab(options = {}) {
    document.body.dataset.activeTab = state.tab;
    all("[data-tab]").forEach((tab) => {
      const active = tab.dataset.tab === state.tab;
      tab.setAttribute("aria-selected", String(active));
      tab.tabIndex = active ? 0 : -1;
      if (active && options.focus) tab.focus();
    });
    const previousIndex = TABS.indexOf(options.previousTab);
    const nextIndex = TABS.indexOf(state.tab);
    const direction = previousIndex < 0 || previousIndex === nextIndex ? 0 : Math.sign(nextIndex - previousIndex);
    all("[data-panel]").forEach((panel) => {
      const active = panel.dataset.panel === state.tab;
      if (!active) {
        cancelMotion(panel);
        panel.hidden = true;
        return;
      }
      panel.hidden = false;
      if (direction) {
        playMotion(panel, [
          { opacity: 0.94, transform: `translate3d(${direction * motion.shiftPx}px, 0, 0)`, filter: `blur(${motion.blurPx}px)` },
          { opacity: 1, transform: "translate3d(0, 0, 0)", filter: "blur(0)" },
        ], motion.pageMs);
      }
    });
  }

  function selectTab(tab, options = {}) {
    if (!TABS.includes(tab)) return;
    const previousTab = state.tab;
    state.tab = tab;
    renderTab({ ...options, previousTab });
    if (tab === "statistics") renderStatistics();
    if (options.updateUrl !== false) updateUrl();
  }

  function escapeHtml(value) {
    return String(value)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;")
      .replaceAll('"', "&quot;")
      .replaceAll("'", "&#39;");
  }

  function syncLiveRequests() {
    if (streamPaused) return;
    displayedRequests = (Array.isArray(state.recentRequests) ? state.recentRequests : [])
      .filter((request) => Number(request.id) > clearedThroughId)
      .slice(-100);
  }

  function renderLiveStream() {
    const body = $("[data-request-stream]");
    if (body) {
      body.innerHTML = displayedRequests.map((request) => {
        const status = Number(request.status) || 0;
        const fallback = request.result === "fallback";
        const failed = request.result === "error";
        const transport = fallback ? `${request.transport} · 回退` : failed ? `${request.transport} · 失败` : request.transport;
        return `<tr><td>${telemetry.formatClock(request.timestampMs)}</td><td><span class="c-request-status c-request-status--${status < 400 && !failed ? "success" : "error"}">${numberFormatter.format(status)}</span></td><td><code>${escapeHtml(request.path)}</code></td><td><strong>${telemetry.formatBytes(request.rawBytes)}</strong><span aria-hidden="true">→</span><strong>${telemetry.formatBytes(request.sentBytes)}</strong></td><td><span class="c-transport${fallback ? " c-transport--fallback" : failed ? " c-transport--error" : ""}">${escapeHtml(transport)}</span></td><td>${telemetry.formatRate(request.rawBytes, request.sentBytes)}</td></tr>`;
      }).join("");
    }
    const empty = $("[data-stream-empty]");
    if (empty) {
      empty.hidden = displayedRequests.length > 0;
      empty.textContent = streamPaused ? "请求流已暂停。" : clearedThroughId > 0 ? "请求流已清空，不影响聚合统计；下一条请求到达时会继续显示。" : "等待第一条真实请求。";
    }
    all("[data-live-count]").forEach((target) => {
      target.textContent = numberFormatter.format(displayedRequests.length);
    });
    const streamState = $("[data-live-stream-state]");
    if (streamState) streamState.classList?.toggle("is-paused", streamPaused);
    const streamLabel = $("[data-live-stream-label]");
    if (streamLabel) streamLabel.textContent = streamPaused ? "已暂停" : "实时更新";
    const toggle = $('[data-action="toggle-stream"]');
    if (toggle) toggle.setAttribute("aria-pressed", String(streamPaused));
    const actionLabel = $("[data-live-action-label]");
    if (actionLabel) actionLabel.textContent = streamPaused ? "继续" : "暂停";
    const terminal = $(".c-terminal__window");
    if (terminal && !streamPaused) terminal.scrollTop = terminal.scrollHeight;
  }

  function syncChartTabStops() {
    const slots = Array.from(all("[data-stat-bars] .c-bar-slot"));
    if (!slots.length) return;
    activeChartIndex = Math.max(0, Math.min(activeChartIndex, slots.length - 1));
    slots.forEach((slot, index) => { slot.tabIndex = index === activeChartIndex ? 0 : -1; });
  }

  function handleChartKeydown(event, currentSlot) {
    const slots = Array.from(all("[data-stat-bars] .c-bar-slot"));
    const currentIndex = slots.indexOf(currentSlot);
    if (currentIndex < 0) return false;
    const keys = { ArrowRight: 1, ArrowLeft: -1, Home: -currentIndex, End: slots.length - 1 - currentIndex };
    if (!Object.hasOwn(keys, event.key)) return false;
    event.preventDefault();
    activeChartIndex = (currentIndex + keys[event.key] + slots.length) % slots.length;
    syncChartTabStops();
    slots[activeChartIndex].focus();
    return true;
  }

  function renderStatistics() {
    const range = Number($('[data-filter="range"]')?.value ?? 1_440);
    const transport = $('[data-filter="transport"]')?.value ?? "all";
    const result = $('[data-filter="result"]')?.value ?? "all";
    const chart = telemetry.selectWindow(state.trafficWindows, range);
    const buckets = chart?.buckets ?? [];
    const totals = telemetry.summarizeBuckets(buckets, transport, result);
    const speed = telemetry.estimateSpeed(buckets, transport, result);
    const values = {
      requests: numberFormatter.format(totals.requests),
      "raw-bytes": telemetry.formatBytes(totals.rawBytes),
      "sent-bytes": telemetry.formatBytes(totals.sentBytes),
      "saved-bytes": telemetry.formatBytes(Math.max(0, totals.rawBytes - totals.sentBytes)),
      "savings-rate": telemetry.formatRate(totals.rawBytes, totals.sentBytes),
      "speed-gain": telemetry.formatSpeedGain(speed),
    };
    all("[data-stat]").forEach((target) => {
      target.textContent = values[target.dataset.stat] ?? "";
    });

    const transportLabel = transport === "all" ? "全部方式" : transport === "WS" ? "WebSocket" : "HTTP";
    const resultLabel = result === "all" ? "全部结果" : result === "success" ? "成功" : result === "fallback" ? "回退" : "失败";
    const summary = $("[data-stats-summary]");
    if (summary) summary.textContent = `${RANGE_LABELS[range] ?? RANGE_LABELS[1440]} / ${transportLabel} / ${resultLabel}`;
    const granularity = $("[data-stat-granularity]");
    if (granularity) granularity.textContent = telemetry.granularityLabel(chart?.bucketSeconds ?? 3_600);

    const bars = $("[data-stat-bars]");
    if (bars) {
      bars.style.setProperty("--chart-bucket-count", String(buckets.length || 1));
      const bucketValues = buckets.map((bucket) => telemetry.bucketTotals(bucket, transport, result));
      const maxRequests = Math.max(...bucketValues.map((bucket) => bucket.requests), 1);
      const markup = buckets.map((bucket, index) => {
        const value = bucketValues[index];
        const saved = Math.max(0, value.rawBytes - value.sentBytes);
        const time = `${telemetry.formatChartTime(bucket.startMs, range)}–${telemetry.formatChartTime(bucket.endMs, range)}`;
        const height = value.requests ? Math.round(12 + value.requests / maxRequests * 88) : 2;
        const sentShare = value.rawBytes ? Math.round(value.sentBytes / value.rawBytes * 100) : 100;
        const tooltipId = `chart-tooltip-${index}`;
        return `<span class="c-bar-slot${value.requests ? "" : " is-empty"}" style="--bar: ${height}%; --sent-share: ${sentShare}%" tabindex="${index === 0 ? 0 : -1}" role="img" aria-describedby="${tooltipId}" aria-label="${time}，${numberFormatter.format(value.requests)} 个请求，发送 ${telemetry.formatBytes(value.sentBytes)}，节省 ${telemetry.formatBytes(saved)}"><i class="c-bar"></i><span id="${tooltipId}" class="c-bar-tooltip" role="tooltip"><strong>时间</strong><span>${time}</span><strong>请求数</strong><span>${numberFormatter.format(value.requests)}</span><strong>发送</strong><span>${telemetry.formatBytes(value.sentBytes)}</span><strong>节省</strong><span>${telemetry.formatBytes(saved)}</span></span></span>`;
      }).join("");
      if (renderedBarMarkup !== markup) {
        bars.innerHTML = markup;
        renderedBarMarkup = markup;
        activeChartIndex = Math.min(activeChartIndex, Math.max(0, buckets.length - 1));
        syncChartTabStops();
      }
    }
    const axis = $("[data-stat-axis]");
    if (axis) {
      if (buckets.length) {
        const middle = buckets[Math.floor(buckets.length / 2)].startMs;
        axis.innerHTML = [buckets[0].startMs, middle, chart.currentPeriodStartMs]
          .map((timestamp) => `<span>${telemetry.formatChartTime(timestamp, range)}</span>`)
          .join("");
      } else axis.innerHTML = "";
    }
    const windows = $("[data-stat-windows]");
    if (windows) {
      windows.innerHTML = ROLLING_WINDOWS.map((minutes) => {
        const window = telemetry.selectWindow(state.trafficWindows, minutes);
        const windowTotals = telemetry.summarizeBuckets(window?.buckets ?? [], transport, result);
        return `<article class="c-window-card"><header><span>${RANGE_LABELS[minutes]}</span><strong>${numberFormatter.format(windowTotals.requests)} 个请求</strong></header><div><p><span>请求数</span><strong>${numberFormatter.format(windowTotals.requests)}</strong></p><p><span>原始 → 发送</span><strong>${telemetry.formatBytes(windowTotals.rawBytes)} → ${telemetry.formatBytes(windowTotals.sentBytes)}</strong></p><p><span>节省率</span><strong>${telemetry.formatRate(windowTotals.rawBytes, windowTotals.sentBytes)}</strong></p></div></article>`;
      }).join("");
    }
    const empty = $("[data-stat-empty]");
    if (empty) empty.hidden = totals.requests > 0;
  }

  function applyStatus(status) {
    if (status && typeof status === "object") state = { ...state, ...status, technicalDetail: "" };
    syncLiveRequests();
    renderState();
  }

  function applyPreviewAction(command, args) {
    state.technicalDetail = "";
    if (command === "set_compression") {
      state.compressionEnabled = args.enabled;
      state.compressionVerified = false;
    }
    if (command === "set_websocket") {
      state.websocketEnabled = args.enabled;
      state.websocketVerified = false;
      state.websocketZstdVerified = false;
      state.websocketState = args.enabled ? "waiting" : "disabled";
      state.restartRequired = true;
    }
    if (command === "set_autostart") state.autostartEnabled = args.enabled;
    if (command === "set_dock_visible") state.dockVisible = args.visible;
    if (command === "restart_codex") {
      state.desktopRestarted = true;
      state.restartRequired = false;
    }
    if (command === "retry_takeover") state.configState = "managed";
    if (command === "set_ai_cove_upstream") {
      state.aiCoveUpstream = true;
      state.aiCoveUpstreamFixAvailable = false;
      state.upstream = "https://api.ai-cove.com/v1";
      state.configState = "managed";
      state.serviceHealthy = true;
    }
    if (command === "confirm_non_ai_cove") state.nonAiCoveConfirmed = true;
    if (command === "check_for_updates") {
      state.updateState = "available";
      state.updateMessage = "Preview：发现可安装的新版本";
    }
    if (command === "install_update") {
      state.updateState = "ready";
      state.updateMessage = "Preview：更新安装流程已完成";
      state.updateProgress = 100;
    }
    state.configMessage = `Preview：${state.configMessage.replace(/^Preview：/, "")}`;
    renderState();
  }

  async function handleAction(action) {
    if (action === "open-config") {
      selectTab("config", { focus: true });
      return;
    }
    if (action === "toggle-stream") {
      streamPaused = !streamPaused;
      if (!streamPaused) syncLiveRequests();
      renderLiveStream();
      return;
    }
    if (action === "clear-stream") {
      clearedThroughId = Math.max(clearedThroughId, ...(state.recentRequests ?? []).map((request) => Number(request.id) || 0));
      displayedRequests = [];
      renderLiveStream();
      return;
    }
    const [command, buildArgs] = actions[action] ?? [];
    if (!command || pendingAction) return;
    const args = buildArgs?.();
    pendingAction = action;
    renderControls();
    try {
      if (invoke) {
        const status = await (args ? invoke(command, args) : invoke(command));
        if (command === "confirm_non_ai_cove") state.nonAiCoveConfirmed = true;
        applyStatus(status);
      } else applyPreviewAction(command, args);
    } catch (error) {
      state.configMessage = "操作未完成，请按提示重试。";
      state.technicalDetail = error instanceof Error ? error.message : String(error);
    } finally {
      pendingAction = "";
      renderState();
    }
  }

  async function refreshStatus() {
    if (!invoke || pendingAction || refreshing) return;
    refreshing = true;
    try {
      const status = await invoke("get_app_status");
      if (!pendingAction) applyStatus(status);
    } catch (error) {
      state.serviceHealthy = false;
      state.configMessage = "无法读取 Turbo 状态，请确认应用仍在运行后重试。";
      state.technicalDetail = error instanceof Error ? error.message : String(error);
      renderState();
    } finally {
      refreshing = false;
    }
  }

  function handleTabKeydown(event, currentTab) {
    const currentIndex = TABS.indexOf(currentTab.dataset.tab);
    const keys = { ArrowRight: 1, ArrowLeft: -1, Home: -currentIndex, End: TABS.length - 1 - currentIndex };
    if (!Object.hasOwn(keys, event.key)) return;
    event.preventDefault();
    selectTab(TABS[(currentIndex + keys[event.key] + TABS.length) % TABS.length], { focus: true });
  }

  function bindDotField() {
    const surface = $(".turbo-panels");
    const canvas = surface?.querySelector("[data-dot-field]");
    const context = canvas?.getContext("2d");
    if (!surface || !canvas || !context) return;
    const reduceMotion = window.matchMedia("(prefers-reduced-motion: reduce)");
    const finePointer = window.matchMedia("(pointer: fine)");
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
      dots.forEach((dot) => {
        context.beginPath();
        context.arc(dot.x + dot.dx, dot.y + dot.dy, 1.2, 0, Math.PI * 2);
        context.fill();
      });
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
      dots.forEach((dot) => {
        const x = dot.x - pointer.x;
        const y = dot.y - pointer.y;
        const distance = Math.hypot(x, y) || 1;
        const influence = pointer.inside && distance < 180 ? (1 - distance / 180) ** 2 * pointer.engagement : 0;
        const targetX = x / distance * 130 * influence;
        const targetY = y / distance * 130 * influence;
        dot.dx += (targetX - dot.dx) * 0.18;
        dot.dy += (targetY - dot.dy) * 0.18;
        if (Math.abs(targetX - dot.dx) + Math.abs(targetY - dot.dy) > 0.04 || Math.abs(dot.dx) + Math.abs(dot.dy) > 0.05) moving = true;
      });
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
      dots.forEach((dot) => {
        dot.dx = 0;
        dot.dy = 0;
      });
      draw();
      canvas.dataset.dotState = "rest";
    });
    resize();
  }

  function init() {
    state.tab = readTab();
    syncLiveRequests();
    document.addEventListener("click", (event) => {
      const action = event.target.closest?.("[data-action]");
      if (action) void handleAction(action.dataset.action);
      const tab = event.target.closest?.("[data-tab]");
      if (tab) selectTab(tab.dataset.tab);
    });
    document.addEventListener("keydown", (event) => {
      const chartSlot = event.target.closest?.(".c-bar-slot");
      if (chartSlot && handleChartKeydown(event, chartSlot)) return;
      const tab = event.target.closest?.("[data-tab]");
      if (tab) handleTabKeydown(event, tab);
    });
    document.addEventListener("focusin", (event) => {
      const chartSlot = event.target.closest?.(".c-bar-slot");
      if (!chartSlot) return;
      const slots = Array.from(all("[data-stat-bars] .c-bar-slot"));
      const index = slots.indexOf(chartSlot);
      if (index >= 0) activeChartIndex = index;
    });
    document.addEventListener("change", (event) => {
      if (event.target.matches?.("[data-filter]")) renderStatistics();
    });
    window.addEventListener("popstate", () => selectTab(readTab(), { updateUrl: false }));
    bindDotField();
    renderTab();
    renderState();
    updateUrl();
    if (invoke) {
      void refreshStatus();
      window.setInterval(refreshStatus, 1_000);
    }
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init, { once: true });
  else init();
})();
