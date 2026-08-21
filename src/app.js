(() => {
  "use strict";

  const TABS = ["live", "statistics", "config"];
  const RANGE_LABELS = { 1: "最近 1 分钟", 10: "最近 10 分钟", 60: "最近 1 小时", 1440: "最近 1 天" };
  const REQUEST_ROUTE_LABELS = {
    hybridWs: "Hybrid WS",
    hybridColdStartHttp: "首轮 HTTP",
    hybridRecoveryHttp: "回退 HTTP",
    directHttp: "压缩 HTTP",
  };
  const ROLLING_WINDOWS = [1, 10, 60, 1440];
  const RECENT_CLOSED_LIMIT = 8;
  const CONNECTION_DENSITIES = ["full", "compact", "state-only"];
  const CONNECTION_DRAG_THRESHOLD_PX = 36;
  const LIVE_TAIL_THRESHOLD_PX = 24;
  const AI_COVE_URL = "https://ai-cove.com";
  const HTTP_DEGRADATION_WINDOW_MS = 5 * 60_000;
  const HTTP_DEGRADATION_MIN_SPAN_MS = 30_000;
  const HTTP_DEGRADATION_MIN_REQUESTS = 5;
  const NETWORK_ERROR_MESSAGE = "请求未能连接到 AI Cove 上游，疑似当前网络或代理异常。\n请尝试切换手机热点排查，如果无法定位请联管理员。";
  const invoke = window.__TAURI__?.core?.invoke;
  const telemetry = window.TurboTelemetry;
  const connectionDom = window.TurboConnectionDOM;
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
    hybridWs: 0,
    hybridColdStartHttp: 0,
    hybridRecoveryHttp: 0,
    directHttp: 0,
    autostartEnabled: true,
    dockVisible: true,
    dockControlAvailable: true,
    codexState: "checking",
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
      { id: 1, ageSeconds: 3, status: 200, path: "/v1/responses", rawBytes: 186_420, sentBytes: 82_110, transport: "WS", result: "success", route: "hybridWs" },
      { id: 2, ageSeconds: 11, status: 200, path: "/v1/responses", rawBytes: 94_280, sentBytes: 51_360, transport: "HTTP", result: "success", route: "hybridColdStartHttp" },
      { id: 3, ageSeconds: 48, status: 201, path: "/v1/files", rawBytes: 128_610, sentBytes: 67_240, transport: "HTTP", result: "success", route: "directHttp" },
      { id: 4, ageSeconds: 210, status: 200, path: "/v1/responses", rawBytes: 121_000, sentBytes: 116_000, transport: "HTTP", result: "fallback", route: "hybridRecoveryHttp" },
      { id: 5, ageSeconds: 1_080, status: 200, path: "/v1/responses", rawBytes: 212_000, sentBytes: 104_000, transport: "WS", result: "success", route: "hybridWs" },
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
    codexState: "active",
    provider: "ai-cove",
    upstream: "https://api.ai-cove.com/v1",
    compressionVerified: true,
    websocketVerified: true,
    websocketZstdVerified: true,
    websocketState: "connected",
    websocketHandshakes: 8,
    websocketMessages: 12,
    httpFallbacks: 2,
    hybridWs: 9,
    hybridColdStartHttp: 1,
    hybridRecoveryHttp: 2,
    directHttp: 3,
    requests: 24,
    rawBytes: 1_840_000,
    sentBytes: 1_060_000,
    compressionRatio: 42.4,
    updateMessage: "Preview：尚未检查更新",
  };
  const previewConnectionSnapshot = {
    currentConnections: 6,
    prewarm: 4,
    boundThreads: [
      { id: "S001", threadId: "thread-7c2a91df", activity: "down", idleSeconds: 0, reclaimPolicy: "threadEnd" },
      { id: "S002", threadId: "thread-7c2a91df", activity: "idle", idleSeconds: 18, reclaimPolicy: "threadEnd" },
    ],
    transitions: [{ id: "S003", threadId: "thread-7c2a91df", connectionId: "S003", label: "恢复绑定连接", stage: "等待可用连接", detail: "上游连接关闭", elapsedSeconds: 2 }],
    recentClosed: [
      { id: "C001", threadId: "thread-7c2a91df", connectionId: "S003", reason: "上游连接关闭", agoSeconds: 12, normal: false },
    ],
  };
  let state = { ...(invoke ? desktopStatus : previewStatus), tab: "live", nonAiCoveConfirmed: false };
  let pendingAction = "";
  let refreshing = false;
  let streamPaused = false;
  let liveTailFollowing = true;
  let unseenLiveRequests = 0;
  let liveStreamChanged = true;
  let clearedThroughId = 0;
  let displayedRequests = [];
  let renderedRequestIds = [];
  let liveStreamHydrated = false;
  let statusHydrated = !invoke;
  let renderedBarMarkup = "";
  let activeChartIndex = 0;
  let connectionPanelOpen = false;
  let connectionPanelTrigger = null;
  let connectionDockOffset = 0;
  let aiCoveBubbleOpen = false;
  let connectionSnapshot = invoke
    ? { currentConnections: 0, prewarm: 0, boundThreads: [], transitions: [], recentClosed: [] }
    : previewConnectionSnapshot;
  let connectionLoading = false;
  let connectionRefreshing = false;
  let connectionHydrated = !invoke;
  let connectionError = "";
  const sessionNumbers = new Map();
  const connectionNumbers = new Map();
  const sessionInfos = new Map();
  const sessionInfoRequests = new Set();
  const closedSessionLayouts = new Map();

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

  function setAiCoveBubbleOpen(open, { restoreFocus = false } = {}) {
    const trigger = $("[data-ai-cove-trigger]");
    const bubble = $("[data-ai-cove-bubble]");
    aiCoveBubbleOpen = Boolean(open);
    trigger?.setAttribute("aria-expanded", String(aiCoveBubbleOpen));
    if (bubble) bubble.hidden = !aiCoveBubbleOpen;
    if (!aiCoveBubbleOpen && restoreFocus) trigger?.focus?.();
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
      current: "已检查",
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
    const codex = String(state.codexState).toLowerCase();
    const pending = {
      checking: "正在检查 Codex 状态",
      restart_required: "需要重启 Codex 才会生效",
      waiting_start: "等待启动 Codex",
      restarting: "正在重启 Codex",
      waiting_request: "Codex 已启动，等待真实请求验证",
      restart_failed: "Codex 重启失败，可重试",
    };
    if (pending[codex]) return pending[codex];
    const httpReady = !state.compressionEnabled || state.compressionVerified;
    const websocketReady = !state.websocketEnabled || (state.websocketVerified && state.websocketZstdVerified);
    if (httpReady && websocketReady) return "HTTP / WebSocket 均已生效";
    return "通道可用，等待真实请求验证";
  }

  function totalRequests() {
    return (Number(state.requests) || 0) + (Number(state.websocketMessages) || 0);
  }

  function runtimeObserved() {
    return totalRequests() > 0 || Number(state.websocketHandshakes) > 0;
  }

  function isNetworkIssue(request) {
    return request?.path === "/v1/responses"
      && request?.route === "directHttp"
      && request?.result === "error"
      && Number(request?.status) === 502;
  }

  function formatCodexState() {
    const labels = {
      checking: "检查中",
      restart_required: "需要重启",
      waiting_start: "等待启动",
      restarting: "正在重启",
      waiting_request: "等待请求",
      active: "已生效",
      restart_failed: "重启失败",
    };
    return labels[String(state.codexState).toLowerCase()] ?? "待确认";
  }

  function codexStatus() {
    const codex = String(state.codexState).toLowerCase();
    if (codex === "active") return "verified";
    if (codex === "restart_required") return "required";
    if (codex === "restart_failed") return "blocked";
    return "waiting";
  }

  function formatState(key) {
    const configState = String(state.configState ?? "unknown").toUpperCase();
    const starting = configState === "STARTING";
    const observed = runtimeObserved();
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
      "hybrid-ws": numberFormatter.format(Number(state.hybridWs) || 0),
      "hybrid-cold-start-http": numberFormatter.format(Number(state.hybridColdStartHttp) || 0),
      "hybrid-recovery-http": numberFormatter.format(Number(state.hybridRecoveryHttp) || 0),
      "direct-http": numberFormatter.format(Number(state.directHttp) || 0),
      autostart: state.autostartEnabled ? "开" : "关",
      dock: state.dockVisible ? "开" : "关",
      restart: pendingAction === "restart-codex" || state.codexState === "restarting"
        ? "正在重启…"
        : state.codexState === "waiting_start" ? "启动 Codex"
          : state.codexState === "restart_failed" ? "重试启动 Codex" : "重启 Codex",
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
      "restart-runtime": formatCodexState(),
      "http-zstd-runtime": !state.compressionEnabled ? "已关闭" : state.compressionVerified ? "已验证" : "待验证",
      "websocket-runtime": !state.websocketEnabled ? "已关闭" : state.websocketVerified ? "已验证" : String(state.websocketState).toLowerCase() === "failed" ? "连接失败" : "待验证",
      "websocket-zstd-runtime": !state.websocketEnabled ? "已关闭" : state.websocketZstdVerified ? "已验证" : "待验证",
      "service-prerequisite": starting ? "检查中" : state.serviceHealthy ? "正常" : "异常",
      "config-prerequisite": starting ? "检查中" : configReady() ? "已生效" : formatConfigState(),
      "restart-prerequisite": formatCodexState(),
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
    if (key.startsWith("restart")) return codexStatus();
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
    if (state.websocketEnabled && state.codexState === "active") {
      const cutoff = Date.now() - HTTP_DEGRADATION_WINDOW_MS;
      const responses = (Array.isArray(state.recentRequests) ? state.recentRequests : [])
        .filter((request) => request.path === "/v1/responses" && Number(request.timestampMs) >= cutoff);
      const latestResponse = responses.at(-1);
      const directHttp = responses.filter((request) => request.route === "directHttp" && !isNetworkIssue(request));
      const timestamps = directHttp.map((request) => Number(request.timestampMs));
      const sustained = directHttp.length >= HTTP_DEGRADATION_MIN_REQUESTS
        && latestResponse?.route === "directHttp"
        && !isNetworkIssue(latestResponse)
        && directHttp.length / responses.length >= 0.8
        && Math.max(...timestamps) - Math.min(...timestamps) >= HTTP_DEGRADATION_MIN_SPAN_MS;
      if (sustained) {
        const websocketRecovered = responses.some((request) => request.route === "hybridWs" && request.result === "success");
        return {
          title: websocketRecovered ? "部分旧任务仍在使用 HTTP" : "Codex 可能仍在使用 HTTP",
          message: websocketRecovered
            ? "Turbo 的 WebSocket 已恢复，但部分旧任务仍停留在 HTTP。建议完成当前操作后重启 Codex。"
            : "Turbo 配置正常，但部分任务近期持续未建立 WebSocket。建议完成当前任务后重启 Codex。",
          action: "restart-codex",
          label: "重启 Codex",
        };
      }
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
    return target.animate(keyframes, { duration, easing: motion.easing });
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
    const updateState = String(state.updateState).toLowerCase();
    const visible = {
      dock: Boolean(state.dockControlAvailable),
      "non-ai-cove": !state.aiCoveUpstream && !state.aiCoveUpstreamFixAvailable,
      "ai-cove-upstream": Boolean(state.aiCoveUpstreamFixAvailable),
      "confirm-non-ai-cove": configState === "warning" && !state.nonAiCoveConfirmed,
      retry: !state.aiCoveUpstreamFixAvailable && (configState === "blocked" || (configState === "conflict" && !state.serviceHealthy)),
      "install-update": ["available", "downloaded", "downloading", "installing", "error"].includes(updateState),
      "update-progress": ["downloading", "installing"].includes(updateState),
    };
    all("[data-visible]").forEach((target) => {
      setConditionalVisibility(target, Boolean(visible[target.dataset.visible]));
    });
  }

  function renderControls() {
    const updateState = String(state.updateState).toLowerCase();
    const updateBusy = ["downloading", "installing"].includes(updateState);
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
        const actionPending = pendingAction === action || (action === "install-update" && updateBusy);
        control.disabled = Boolean(pendingAction) || actionPending;
        control.dataset.status = actionPending ? "pending" : "idle";
        control.setAttribute("aria-busy", String(actionPending));
      }
      if (action === "install-update") {
        control.textContent = pendingAction === action || updateBusy
          ? "更新中"
          : updateState === "error" ? "重新下载" : "安装更新";
      }
      if (action === "restart-codex") {
        const codex = String(state.codexState).toLowerCase();
        control.dataset.required = String(["restart_required", "waiting_start", "restart_failed"].includes(codex));
        if (Object.hasOwn(control.dataset, "restartHint")) {
          const title = codex === "waiting_start"
            ? "Codex 尚未运行，启动后会加载 Turbo 配置。"
            : codex === "restart_failed"
              ? "上次启动未完成，可以重试或手动打开 Codex。"
              : "配置已写入，重启后会重新验证传输通道。";
          control.setAttribute("title", title);
          control.setAttribute("aria-label", `${formatState("restart")}：${title}`);
        }
      }
      if (Object.hasOwn(pressed, action)) {
        control.dataset.enabled = String(pressed[action]);
        control.setAttribute("aria-pressed", String(pressed[action]));
      }
    });
  }

  function renderState(options = {}) {
    document.body.dataset.serviceHealthy = String(Boolean(state.serviceHealthy));
    all("[data-state]").forEach((target) => {
      const key = target.dataset.state;
      target.textContent = formatState(key);
      const status = statusFor(key);
      if (status) target.dataset.status = status;
    });
    const updateProgress = Math.max(0, Math.min(100, Number(state.updateProgress) || 0));
    all("[data-state-progress]").forEach((target) => {
      target.style.setProperty("--progress", String(updateProgress / 100));
    });
    all('[role="progressbar"]').forEach((target) => {
      target.setAttribute("aria-valuenow", String(updateProgress));
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
    if (state.tab === "live" && statusHydrated) renderLiveStream(options);
    if (state.tab === "statistics") renderStatistics();
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
      const motionTarget = panel.dataset.panel === "live" ? panel.querySelector?.(".c-console") || panel : panel;
      if (!active) {
        cancelMotion(motionTarget);
        panel.hidden = true;
        return;
      }
      panel.hidden = false;
      if (direction) {
        playMotion(motionTarget, [
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
    if (tab === "live") renderLiveStream({ animateNew: false });
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

  function positionNetworkTooltip(trigger) {
    const tooltip = document.getElementById?.(trigger.getAttribute?.("aria-describedby"));
    if (!tooltip) return;
    const triggerBounds = trigger.getBoundingClientRect();
    const tooltipBounds = tooltip.getBoundingClientRect();
    const viewportMargin = 16;
    const gap = 9;
    const width = Math.min(tooltipBounds.width, window.innerWidth - viewportMargin * 2);
    const left = Math.max(viewportMargin, Math.min(triggerBounds.right - width, window.innerWidth - width - viewportMargin));
    const preferredTop = triggerBounds.top - tooltipBounds.height - gap;
    const top = preferredTop >= 70 ? preferredTop : triggerBounds.bottom + gap;
    tooltip.style.setProperty("--c-network-tooltip-left", `${left}px`);
    tooltip.style.setProperty("--c-network-tooltip-top", `${top}px`);
  }

  function normalizeConnectionSnapshot(snapshot) {
    const prewarm = Math.max(0, Number(snapshot?.prewarm) || 0);
    const boundThreads = Array.isArray(snapshot?.boundThreads) ? snapshot.boundThreads : [];
    const currentConnections = Number(snapshot?.currentConnections);
    return {
      currentConnections: Number.isFinite(currentConnections)
        ? Math.max(0, currentConnections)
        : prewarm + boundThreads.length,
      prewarm,
      boundThreads,
      transitions: Array.isArray(snapshot?.transitions) ? snapshot.transitions : [],
      recentClosed: Array.isArray(snapshot?.recentClosed) ? snapshot.recentClosed : [],
    };
  }

  function connectionIcon(status) {
    return `<i class="c-ws-icon" data-connection-state="${escapeHtml(status)}" aria-hidden="true"></i>`;
  }

  function sessionIcon(status, isSubagent = false) {
    const branch = isSubagent
      ? '<circle class="c-session-icon__badge" cx="10.75" cy="3.25" r="2.35" /><path class="c-session-icon__branch" d="M9.75 2.4v1.65h2m0 0V5.1" />'
      : "";
    const kind = isSubagent ? "subagent" : "primary";
    return `<svg class="c-session-icon" data-connection-state="${escapeHtml(status)}" data-session-kind="${kind}" viewBox="0 0 14 14" aria-hidden="true"><path d="M3 1.75h8a2 2 0 0 1 2 2v4.5a2 2 0 0 1-2 2H7l-3.25 2v-2H3a2 2 0 0 1-2-2v-4.5a2 2 0 0 1 2-2Z" />${branch}</svg>`;
  }

  function connectionThreadId(item) {
    return String(item?.threadId || "").trim();
  }

  function observedConnectionId(item) {
    return String(item?.connectionId || item?.id || "").trim();
  }

  function reconcileConnectionNumbers(snapshot, recentClosed) {
    const items = [...snapshot.boundThreads, ...snapshot.transitions, ...recentClosed];
    const visibleThreadIds = [];
    const visibleThreads = new Set();
    items.forEach((item) => {
      const threadId = connectionThreadId(item);
      if (!threadId || visibleThreads.has(threadId)) return;
      visibleThreads.add(threadId);
      visibleThreadIds.push(threadId);
    });

    Array.from(sessionNumbers.keys()).forEach((threadId) => {
      if (visibleThreads.has(threadId)) return;
      sessionNumbers.delete(threadId);
      connectionNumbers.delete(threadId);
    });

    const usedSessionNumbers = new Set(sessionNumbers.values());
    visibleThreadIds.forEach((threadId) => {
      if (sessionNumbers.has(threadId)) return;
      let number = 1;
      while (usedSessionNumbers.has(number)) number += 1;
      sessionNumbers.set(threadId, number);
      usedSessionNumbers.add(number);
    });

    items.forEach((item) => {
      const threadId = connectionThreadId(item);
      const connectionId = observedConnectionId(item);
      if (!threadId || !connectionId) return;
      let numbers = connectionNumbers.get(threadId);
      if (!numbers) {
        numbers = { next: 1, byId: new Map() };
        connectionNumbers.set(threadId, numbers);
      }
      if (!numbers.byId.has(connectionId)) {
        numbers.byId.set(connectionId, numbers.next);
        numbers.next += 1;
      }
    });
  }

  function sessionName(threadId) {
    return `会话 ${String(sessionNumbers.get(threadId) || 0).padStart(2, "0")}`;
  }

  function sessionTitle(threadId) {
    if (!invoke) return "Preview 会话";
    if (!sessionInfos.has(threadId)) return "读取中…";
    return sessionInfos.get(threadId)?.name || "-";
  }

  function sessionIdentityDetails(threadId) {
    const info = sessionInfos.get(threadId);
    const details = [["会话名称", sessionTitle(threadId)]];
    if (info?.isSubagent) {
      details.push(["会话类型", "子会话"], ["所属父会话", info.parentName || "-"]);
    }
    return details;
  }

  function requestSessionInfos(snapshot, recentClosed) {
    if (!invoke) return;
    const items = [...snapshot.boundThreads, ...snapshot.transitions, ...recentClosed];
    new Set(items.map(connectionThreadId).filter(Boolean)).forEach((threadId) => {
      if (sessionInfoRequests.has(threadId)) return;
      sessionInfoRequests.add(threadId);
      void invoke("get_codex_thread_info", { threadId })
        .then((info) => {
          sessionInfos.set(threadId, info && typeof info === "object" ? {
            name: String(info.name || "").trim(),
            parentName: String(info.parentName || "").trim(),
            isSubagent: Boolean(info.isSubagent),
          } : null);
          if (connectionPanelOpen) renderConnectionInspector();
        })
        .catch(() => sessionInfos.set(threadId, null));
    });
  }

  function connectionShortName(threadId, connectionId) {
    const number = connectionNumbers.get(threadId)?.byId.get(connectionId) || 0;
    return String(number).padStart(2, "0");
  }

  function connectionName(threadId, connectionId) {
    return `连接 ${connectionShortName(threadId, connectionId)}`;
  }

  function groupConnectionsByThread(items) {
    const groups = new Map();
    items.forEach((item) => {
      const threadId = connectionThreadId(item);
      if (!threadId) return;
      if (!groups.has(threadId)) groups.set(threadId, { threadId, items: [] });
      groups.get(threadId).items.push(item);
    });
    return Array.from(groups.values()).sort(
      (left, right) => sessionNumbers.get(left.threadId) - sessionNumbers.get(right.threadId),
    );
  }

  function sortConnectionsByNumber(group) {
    return [...group.items].sort((left, right) => {
      const numbers = connectionNumbers.get(group.threadId)?.byId;
      return (numbers?.get(observedConnectionId(left)) || 0) - (numbers?.get(observedConnectionId(right)) || 0);
    });
  }

  function boundSessionStatus(group) {
    return group.items.some((item) => ["up", "down"].includes(item.activity)) ? "active" : "bound";
  }

  function connectionDensity(count) {
    if (count <= 1) return "full";
    return count === 2 ? "compact" : "state-only";
  }

  function formatConnectionAge(seconds) {
    const value = Math.max(0, Math.floor(Number(seconds) || 0));
    if (value < 60) return `${value} 秒`;
    if (value < 3_600) return `${Math.floor(value / 60)} 分钟`;
    return `${Math.floor(value / 3_600)} 小时`;
  }

  function connectionActivityGlyph(activity) {
    if (activity === "idle") return '<svg class="c-connection-idle" viewBox="0 0 18 14" data-direction="up-right" aria-hidden="true"><path d="M.75 10.5h3l-3 2.5h3" /><path d="M5 5.75h4.25L5 9.25h4.25" /><path d="M10.5.75h6l-6 5h6" /></svg>';
    const direction = activity === "up" ? "up" : "down";
    const path = direction === "up" ? "M6 10V2M3 5l3-3 3 3" : "M6 2v8m3-3-3 3-3-3";
    return `<svg class="c-connection-activity" data-direction="${direction}" viewBox="0 0 12 12" aria-hidden="true"><path d="${path}" /></svg>`;
  }

  function renderHoverDetail(title, details) {
    const rows = details.map(([label, value]) => `<div><dt>${escapeHtml(label)}</dt><dd>${escapeHtml(value)}</dd></div>`).join("");
    return `<span class="c-hover-card" aria-hidden="true"><strong>${escapeHtml(title)}</strong><dl>${rows}</dl></span>`;
  }

  function renderConnectionChip({ status, name, details, glyph = "", connectionId = "", eventId = "", shortName = "" }) {
    const label = `${name}，${details.map(([key, value]) => `${key} ${value}`).join("，")}`;
    const connectionAttribute = connectionId ? ` data-connection-id="${escapeHtml(connectionId)}"` : "";
    const eventAttribute = eventId ? ` data-connection-event-id="${escapeHtml(eventId)}"` : "";
    const key = eventId ? `event:${eventId}` : connectionId ? `connection:${connectionId}` : `prewarm:${name}`;
    const shortNameAttribute = shortName ? ` data-short-name="${escapeHtml(shortName)}"` : "";
    return `<span class="c-connection-chip" tabindex="0" data-connection-key="${escapeHtml(key)}"${connectionAttribute}${eventAttribute} aria-label="${escapeHtml(label)}">${connectionIcon(status)}<strong${shortNameAttribute}>${escapeHtml(name)}</strong>${glyph}${renderHoverDetail(name, details)}</span>`;
  }

  function renderBoundSession(group) {
    const items = sortConnectionsByNumber(group);
    const info = sessionInfos.get(group.threadId);
    const counts = items.reduce((result, item) => {
      const activity = ["up", "down"].includes(item.activity) ? item.activity : "idle";
      result[activity] += 1;
      return result;
    }, { up: 0, down: 0, idle: 0 });
    const name = sessionName(group.threadId);
    const details = items.map((item) => {
      const activity = ["up", "down"].includes(item.activity) ? item.activity : "idle";
      const activityLabel = activity === "up"
        ? "正在发送"
        : activity === "down"
          ? "正在接收"
          : `空闲 ${formatConnectionAge(item.idleSeconds)}`;
      const reclaim = item.reclaimPolicy === "threadEnd" ? "随线程结束回收" : "按连接策略回收";
      const connectionId = observedConnectionId(item);
      return renderConnectionChip({
        status: activity === "idle" ? "bound" : "active",
        name: connectionName(group.threadId, connectionId),
        details: [
          ["状态", activityLabel],
          ["所属会话", name],
          ["连接 ID", connectionId],
          ["回收", reclaim],
        ],
        glyph: connectionActivityGlyph(activity),
        connectionId,
        shortName: connectionShortName(group.threadId, connectionId),
      });
    }).join("");
    const sessionDetails = [
      ...sessionIdentityDetails(group.threadId),
      ["连接", `${items.length} 条`],
      ["传输", `发送 ${counts.up} · 接收 ${counts.down} · 空闲 ${counts.idle}`],
    ];
    const summary = `${name}，${info?.isSubagent ? "子会话，" : ""}${items.length} 条连接，发送 ${counts.up}，接收 ${counts.down}，空闲 ${counts.idle}`;
    return `<div class="c-connection-session c-connection-session--bound" role="group" data-connection-key="bound:${escapeHtml(group.threadId)}" data-density="${connectionDensity(items.length)}" data-thread-id="${escapeHtml(group.threadId)}" aria-label="${escapeHtml(summary)}"><div class="c-connection-session__summary" tabindex="0" aria-label="${escapeHtml(summary)}">${sessionIcon(boundSessionStatus(group), Boolean(info?.isSubagent))}<strong>${name}</strong><span class="c-connection-session__count">×${items.length}</span>${renderHoverDetail(name, sessionDetails)}</div><span class="c-connection-session__separator" aria-hidden="true"></span><div class="c-connection-session__connections">${details}</div></div>`;
  }

  function renderClosedSession(group, status) {
    const name = sessionName(group.threadId);
    const items = sortConnectionsByNumber(group);
    const info = sessionInfos.get(group.threadId);
    const events = items.map((item) => {
      const connectionId = observedConnectionId(item);
      return renderConnectionChip({
        status: item.normal ? "closed" : "error",
        name: connectionName(group.threadId, connectionId),
        details: [
          ["结果", item.normal ? "正常关闭" : "异常关闭"],
          ["原因", item.reason],
          ["关闭于", `${formatConnectionAge(item.agoSeconds)}前`],
          ["所属会话", name],
          ["连接 ID", connectionId],
        ],
        connectionId,
        eventId: item.id,
        shortName: connectionShortName(group.threadId, connectionId),
      });
    }).join("");
    const abnormalCount = items.filter((item) => !item.normal).length;
    const sessionState = status === "closed" ? "已释放" : status === "pending" ? "恢复中" : "仍在绑定";
    const sessionDetails = [
      ...sessionIdentityDetails(group.threadId),
      ["会话状态", sessionState],
      ["关闭记录", `${items.length} 条`],
      ["异常", `${abnormalCount} 条`],
    ];
    const summary = `${name}，${info?.isSubagent ? "子会话，" : ""}${sessionState}，${items.length} 条近期关闭记录，异常 ${abnormalCount}`;
    const density = closedSessionLayouts.get(group.threadId)?.density || "full";
    return `<div class="c-connection-session c-connection-session--closed" role="group" data-connection-key="closed:${escapeHtml(group.threadId)}" data-auto-density data-density="${density}" data-thread-id="${escapeHtml(group.threadId)}" aria-label="${escapeHtml(summary)}"><div class="c-connection-session__summary" tabindex="0" aria-label="${escapeHtml(summary)}">${sessionIcon(status, Boolean(info?.isSubagent))}<strong>${name}</strong><span class="c-connection-session__count">×${items.length}</span>${renderHoverDetail(name, sessionDetails)}</div><span class="c-connection-session__separator" aria-hidden="true"></span><div class="c-connection-session__connections">${events}</div></div>`;
  }

  function closedConnectionsFit(connections) {
    const chips = Array.from(connections.children ?? []);
    const rowGap = Number.parseFloat(getComputedStyle(connections).columnGap) || 0;
    const requiredWidth = chips.reduce((total, chip) => {
      const style = getComputedStyle(chip);
      const parts = Array.from(chip.children ?? []).filter((part) => {
        const partStyle = getComputedStyle(part);
        return partStyle.display !== "none" && partStyle.position !== "absolute";
      });
      const innerGap = (Number.parseFloat(style.columnGap) || 0) * Math.max(0, parts.length - 1);
      const inset = [style.paddingLeft, style.paddingRight, style.borderLeftWidth, style.borderRightWidth]
        .reduce((sum, value) => sum + (Number.parseFloat(value) || 0), 0);
      const contentWidth = parts.reduce((sum, part) => {
        const visibleWidth = part.getBoundingClientRect().width;
        return sum + Math.max(Number(part.scrollWidth) || 0, visibleWidth);
      }, 0);
      return total + contentWidth + innerGap + inset;
    }, rowGap * Math.max(0, chips.length - 1));
    return requiredWidth <= Number(connections.clientWidth) + 0.5;
  }

  function fitClosedSessionDensities(list = $('[data-connection-list="closed"]')) {
    if (typeof getComputedStyle !== "function") return;
    Array.from(list?.querySelectorAll?.("[data-auto-density]") ?? []).forEach((session) => {
      const connections = session.querySelector?.(".c-connection-session__connections");
      if (!connections || !Number(connections.clientWidth)) return;
      const signature = Array.from(connections.children ?? [], (chip) => chip.children?.[1]?.dataset?.shortName || "").join(",");
      const layoutKey = `${connections.clientWidth}:${signature || connections.children?.length || 0}`;
      const cached = closedSessionLayouts.get(session.dataset.threadId);
      if (cached?.key === layoutKey) {
        session.dataset.density = cached.density;
        return;
      }
      const density = CONNECTION_DENSITIES.find((candidate) => {
        session.dataset.density = candidate;
        return closedConnectionsFit(connections);
      }) || "state-only";
      session.dataset.density = density;
      if (session.dataset.threadId) closedSessionLayouts.set(session.dataset.threadId, { key: layoutKey, density });
    });
  }

  function renderTransitionDetails(item, identityDetail) {
    return `<dl><div><dt>操作</dt><dd>${escapeHtml(item.label)}</dd></div><div><dt>身份</dt><dd>${escapeHtml(identityDetail)}</dd></div><div><dt>已用时</dt><dd>${formatConnectionAge(item.elapsedSeconds)}</dd></div><div><dt>详情</dt><dd>${escapeHtml(item.detail)}</dd></div></dl>`;
  }

  function renderTransitionItem(item, threadId) {
    const connectionId = observedConnectionId(item);
    const identity = `${sessionName(threadId)} · ${connectionName(threadId, connectionId)}`;
    const attributes = ` data-thread-id="${escapeHtml(threadId)}" data-connection-id="${escapeHtml(connectionId)}"`;
    const summary = `${identity}，${item.label}，${item.stage}`;
    return `<details class="c-connection-transition" data-connection-key="transition:${escapeHtml(item.id)}" data-transition-id="${escapeHtml(item.id)}"${attributes}><summary aria-label="${escapeHtml(summary)}">${connectionIcon("pending")}<strong>${escapeHtml(identity)}</strong><span>${escapeHtml(item.stage)}</span></summary>${renderTransitionDetails(item, connectionId)}</details>`;
  }

  function renderPoolTransition(item) {
    return `<details class="c-connection-transition" data-connection-key="transition:${escapeHtml(item.id)}" data-transition-id="${escapeHtml(item.id)}"><summary>${connectionIcon("pending")}<strong>${escapeHtml(item.label)}</strong><span>${escapeHtml(item.stage)}</span></summary>${renderTransitionDetails(item, item.id)}</details>`;
  }

  function renderConnectionInspector({ force = false } = {}) {
    const panel = $("[data-connection-panel]");
    if (!panel) return;
    const snapshot = normalizeConnectionSnapshot(connectionSnapshot);
    const recentClosed = snapshot.recentClosed.slice(0, RECENT_CLOSED_LIMIT);
    reconcileConnectionNumbers(snapshot, recentClosed);
    requestSessionInfos(snapshot, recentClosed);
    const activityCounts = snapshot.boundThreads.reduce((counts, item) => {
      const activity = ["up", "down"].includes(item.activity) ? item.activity : "idle";
      counts[activity] += 1;
      return counts;
    }, { up: 0, down: 0, idle: 0 });
    Object.entries(activityCounts).forEach(([activity, count]) => {
      const target = $(`[data-connection-summary="${activity}"]`);
      if (target) target.textContent = numberFormatter.format(count);
    });
    const summaryLabel = `发送 ${activityCounts.up}，接收 ${activityCounts.down}，休眠 ${activityCounts.idle}`;
    const summaryTrigger = $("[data-connection-summary-trigger]");
    if (summaryTrigger) {
      const actionLabel = connectionPanelOpen
        ? "点击或上拉收起连接检查器，左右拖动可移动"
        : "左右拖动移动，点击或下拉展开连接检查器";
      summaryTrigger.setAttribute("aria-label", `${summaryLabel}，${actionLabel}`);
      summaryTrigger.dataset.tooltip = "左右拖动移动 · 点击或下拉展开";
      if (connectionPanelOpen) summaryTrigger.setAttribute("title", "左右拖动移动 · 点击或上拉收起");
      else summaryTrigger.removeAttribute?.("title");
    }
    if (!connectionPanelOpen && !force) return;
    const boundGroups = groupConnectionsByThread(snapshot.boundThreads);
    const sessionStatuses = new Map(
      boundGroups.map((group) => [group.threadId, boundSessionStatus(group)]),
    );
    snapshot.transitions.forEach((item) => {
      const threadId = connectionThreadId(item);
      if (threadId && !sessionStatuses.has(threadId)) sessionStatuses.set(threadId, "pending");
    });
    const total = $("[data-connection-total]");
    if (total) total.textContent = numberFormatter.format(snapshot.currentConnections);

    const counts = {
      prewarm: snapshot.prewarm,
      bound: snapshot.boundThreads.length,
      transitions: snapshot.transitions.length,
      closed: recentClosed.length,
    };
    Object.entries(counts).forEach(([group, count]) => {
      const target = $(`[data-connection-count="${group}"]`);
      if (target) target.textContent = numberFormatter.format(count);
    });

    const prewarm = $("[data-connection-list=\"prewarm\"]");
    if (prewarm) {
      connectionDom.reconcileList(prewarm, snapshot.prewarm
        ? Array.from({ length: snapshot.prewarm }, (_, index) => renderConnectionChip({
          status: "warm",
          name: `P${String(index + 1).padStart(2, "0")}`,
          details: [
            ["状态", "空白预热"],
            ["回收", "容量压力时回收"],
          ],
        })).join("")
        : '<span class="c-connection-empty">暂无可用连接</span>');
    }

    const bound = $("[data-connection-list=\"bound\"]");
    if (bound) {
      connectionDom.reconcileList(bound, snapshot.boundThreads.length
        ? boundGroups.map(renderBoundSession).join("")
        : '<span class="c-connection-empty">暂无绑定会话</span>');
    }

    const transitions = $("[data-connection-list=\"transitions\"]");
    if (transitions) {
      const expanded = new Set(Array.from(transitions.querySelectorAll?.("details[open][data-transition-id]") ?? [], (item) => item.dataset.transitionId));
      connectionDom.reconcileList(transitions, snapshot.transitions.length
        ? snapshot.transitions.map((item) => {
          const threadId = connectionThreadId(item);
          return threadId ? renderTransitionItem(item, threadId) : renderPoolTransition(item);
        }).join("")
        : '<span class="c-connection-empty">当前没有连接操作</span>');
      Array.from(transitions.querySelectorAll?.("details[data-transition-id]") ?? []).forEach((item) => {
        item.open = expanded.has(item.dataset.transitionId);
      });
    }

    const closed = $("[data-connection-list=\"closed\"]");
    if (closed) {
      connectionDom.reconcileList(closed, recentClosed.length
        ? groupConnectionsByThread(recentClosed)
          .map((group) => renderClosedSession(group, sessionStatuses.get(group.threadId) || "closed"))
          .join("")
        : '<span class="c-connection-empty">暂无关闭记录</span>');
      fitClosedSessionDensities(closed);
    }

    const message = $("[data-connection-message]");
    if (message) {
      message.hidden = !connectionLoading && !connectionError;
      message.textContent = connectionError
        ? `连接状态读取失败：${connectionError}`
        : connectionLoading
          ? "正在读取当前 WebSocket 连接…"
          : "";
    }
  }

  function setConnectionPanelOpen(open, { restoreFocus = true, trigger = null, fromProgress = null } = {}) {
    const summary = $("[data-connection-summary-trigger]");
    const panel = $("[data-connection-panel]");
    const dock = $("[data-connection-dock]");
    const nextOpen = Boolean(open);
    const dragProgress = Number.isFinite(fromProgress) ? Math.max(0, Math.min(1, fromProgress)) : null;
    const panelWasVisible = Boolean(panel && !panel.hidden);
    const previousSummaryTop = summary?.getBoundingClientRect?.().top;
    cancelMotion(summary);
    cancelMotion(panel);
    panel?.classList?.remove?.("is-closing");
    panel?.classList?.remove?.("is-drag-preview");
    if (summary?.style) summary.style.transform = "";
    if (panel?.style) {
      panel.style.clipPath = "";
      panel.style.opacity = "";
      panel.style.filter = "";
    }
    connectionPanelOpen = nextOpen;
    if (connectionPanelOpen && trigger) connectionPanelTrigger = trigger;
    all('[data-action="toggle-connections"]').forEach((control) => {
      control.setAttribute("aria-expanded", String(connectionPanelOpen));
    });
    if (panel && connectionPanelOpen) panel.hidden = false;
    if (panel && !connectionPanelOpen && panelWasVisible) panel.classList?.add?.("is-closing");
    panel?.setAttribute?.("aria-hidden", String(!connectionPanelOpen));
    if (dock) {
      dock.dataset.open = String(connectionPanelOpen);
      dock.dataset.phase = connectionPanelOpen ? "opening" : panelWasVisible ? "closing" : "closed";
    }
    renderConnectionInspector();
    const nextSummaryTop = summary?.getBoundingClientRect?.().top;
    if (Number.isFinite(previousSummaryTop) && Number.isFinite(nextSummaryTop)) {
      const shift = previousSummaryTop - nextSummaryTop;
      if (Math.abs(shift) > 0.5) {
        playMotion(summary, [
          { transform: `translate3d(0, ${shift}px, 0)` },
          { transform: `translate3d(0, ${shift * 0.18}px, 0)`, offset: 0.78 },
          { transform: "translate3d(0, 0, 0)" },
        ], motion.pageMs);
      }
    }
    if (panel) {
      if (connectionPanelOpen) {
        const keyframes = dragProgress === null ? [
          { opacity: 0.56, clipPath: "inset(0 0 100% 0 round 14px)", filter: "blur(2px)" },
          { opacity: 0.96, clipPath: "inset(0 0 18% 0 round 14px)", filter: "blur(0.5px)", offset: 0.78 },
          { opacity: 1, clipPath: "inset(0 0 0 0 round 14px)", filter: "blur(0)" },
        ] : [
          { opacity: 0.45 + 0.55 * dragProgress, clipPath: `inset(0 0 ${(1 - dragProgress) * 100}% 0 round 14px)`, filter: `blur(${(1 - dragProgress) * 2}px)` },
          { opacity: 1, clipPath: "inset(0 0 0 0 round 14px)", filter: "blur(0)" },
        ];
        const animation = playMotion(panel, keyframes, motion.pageMs);
        const finishOpening = () => {
          if (connectionPanelOpen && dock) dock.dataset.phase = "open";
        };
        if (animation) animation.onfinish = finishOpening;
        else finishOpening();
        summary?.focus?.();
      } else if (panelWasVisible) {
        const keyframes = dragProgress === null ? [
          { opacity: 1, clipPath: "inset(0 0 0 0 round 14px)", filter: "blur(0)" },
          { opacity: 0.7, clipPath: "inset(0 0 82% 0 round 14px)", filter: "blur(1px)", offset: 0.78 },
          { opacity: 0.45, clipPath: "inset(0 0 100% 0 round 14px)", filter: "blur(2px)" },
        ] : [
          { opacity: 0.45 + 0.55 * dragProgress, clipPath: `inset(0 0 ${(1 - dragProgress) * 100}% 0 round 14px)`, filter: `blur(${(1 - dragProgress) * 2}px)` },
          { opacity: 0.45, clipPath: "inset(0 0 100% 0 round 14px)", filter: "blur(2px)" },
        ];
        const animation = playMotion(panel, keyframes, motion.pageMs);
        const finishClosing = () => {
          if (connectionPanelOpen) return;
          panel.hidden = true;
          panel.classList?.remove?.("is-closing");
          if (dock) dock.dataset.phase = "closed";
        };
        if (animation) animation.onfinish = finishClosing;
        else finishClosing();
      } else {
        panel.hidden = true;
        if (dock) dock.dataset.phase = "closed";
      }
    }
    if (!connectionPanelOpen && restoreFocus) connectionPanelTrigger?.focus?.();
  }

  function setConnectionDockOffset(value) {
    const dock = $("[data-connection-dock]");
    if (!dock) return;
    const viewportWidth = Number(window.innerWidth) || document.documentElement?.clientWidth || 0;
    const dockWidth = Number(dock.offsetWidth) || dock.getBoundingClientRect?.().width || 0;
    const limit = Math.max(0, (viewportWidth - dockWidth) / 2 - 12);
    connectionDockOffset = Math.max(-limit, Math.min(limit, Number(value) || 0));
    dock.style.setProperty("--connection-dock-offset", `${Math.round(connectionDockOffset)}px`);
  }

  function bindConnectionDock() {
    const dock = $("[data-connection-dock]");
    const grip = $("[data-connection-grip]");
    if (!dock || !grip) return;
    let drag = null;
    let suppressClick = false;

    const applyVerticalProgress = (progress) => {
      const value = Math.max(0, Math.min(1, progress));
      const hiddenPercent = Math.round((1 - value) * 1_000) / 10;
      drag.progress = value;
      grip.style.transform = `translate3d(0, ${Math.round(drag.panelHeight * value)}px, 0)`;
      drag.panel.style.clipPath = `inset(0 0 ${hiddenPercent}% 0 round 14px)`;
      drag.panel.style.opacity = String(0.45 + 0.55 * value);
      drag.panel.style.filter = `blur(${(1 - value) * 2}px)`;
    };

    const startVerticalDrag = () => {
      const panel = $("[data-connection-panel]");
      if (!panel) return false;
      cancelMotion(grip);
      cancelMotion(panel);
      panel.classList?.remove?.("is-closing");
      panel.hidden = false;
      renderConnectionInspector({ force: true });
      drag.panel = panel;
      drag.panelHeight = Math.max(1, Number(panel.getBoundingClientRect?.().height) || Number(panel.scrollHeight) || 1);
      panel.classList?.add?.("is-drag-preview");
      panel.setAttribute?.("aria-hidden", String(!drag.startedOpen));
      dock.dataset.phase = drag.startedOpen ? "dragging-close" : "dragging-open";
      applyVerticalProgress(drag.startedOpen ? 1 : 0);
      return true;
    };

    grip.addEventListener("pointerdown", (event) => {
      if (event.button !== 0) return;
      drag = {
        pointerId: event.pointerId,
        startX: event.clientX,
        startY: event.clientY,
        lastY: event.clientY,
        startOffset: connectionDockOffset,
        startedOpen: connectionPanelOpen,
        axis: null,
        progress: connectionPanelOpen ? 1 : 0,
        moved: false,
      };
      dock.classList.add("is-dragging");
      grip.setPointerCapture?.(event.pointerId);
    });
    grip.addEventListener("pointermove", (event) => {
      if (!drag || event.pointerId !== drag.pointerId) return;
      const deltaX = event.clientX - drag.startX;
      const deltaY = event.clientY - drag.startY;
      drag.lastY = event.clientY;
      if (!drag.axis && Math.hypot(deltaX, deltaY) > 3) {
        drag.axis = Math.abs(deltaX) > Math.abs(deltaY) ? "x" : "y";
        if (drag.axis === "y" && !startVerticalDrag()) drag.axis = "x";
      }
      if (!drag.axis) return;
      drag.moved = true;
      if (drag.axis === "x") setConnectionDockOffset(drag.startOffset + deltaX);
      else applyVerticalProgress((drag.startedOpen ? 1 : 0) + deltaY / drag.panelHeight);
      event.preventDefault();
    });
    const finishDrag = (event) => {
      if (!drag || event.pointerId !== drag.pointerId) return;
      const completed = drag;
      const endY = Number.isFinite(event.clientY) ? event.clientY : completed.lastY;
      suppressClick = completed.moved;
      if (suppressClick) window.setTimeout?.(() => { suppressClick = false; }, 0);
      dock.classList.remove("is-dragging");
      grip.releasePointerCapture?.(event.pointerId);
      drag = null;
      if (completed.axis === "y") {
        const distance = endY - completed.startY;
        const targetOpen = event.type === "pointercancel"
          ? completed.startedOpen
          : completed.startedOpen
            ? distance > -CONNECTION_DRAG_THRESHOLD_PX
            : distance >= CONNECTION_DRAG_THRESHOLD_PX;
        setConnectionPanelOpen(targetOpen, {
          trigger: targetOpen && !completed.startedOpen ? grip : null,
          fromProgress: completed.progress,
        });
      }
    };
    grip.addEventListener("pointerup", finishDrag);
    grip.addEventListener("pointercancel", finishDrag);
    grip.addEventListener("click", (event) => {
      if (!suppressClick) return;
      suppressClick = false;
      event.preventDefault();
      event.stopPropagation?.();
    });
    grip.addEventListener("keydown", (event) => {
      if (event.key === "ArrowUp" && connectionPanelOpen) setConnectionPanelOpen(false);
      else if (event.key === "ArrowLeft") setConnectionDockOffset(connectionDockOffset - 24);
      else if (event.key === "ArrowRight") setConnectionDockOffset(connectionDockOffset + 24);
      else if (event.key === "Home") setConnectionDockOffset(0);
      else return;
      event.preventDefault();
    });
    window.addEventListener("resize", () => {
      setConnectionDockOffset(connectionDockOffset);
      fitClosedSessionDensities();
    }, { passive: true });
    setConnectionDockOffset(0);
  }

  async function refreshConnectionSnapshot() {
    if (state.tab !== "live" || connectionRefreshing || document.hidden) return;
    if (!invoke) {
      connectionSnapshot = previewConnectionSnapshot;
      connectionHydrated = true;
      connectionError = "";
      renderConnectionInspector();
      return;
    }
    connectionRefreshing = true;
    connectionLoading = connectionPanelOpen && !connectionHydrated;
    connectionError = "";
    renderConnectionInspector();
    try {
      connectionSnapshot = normalizeConnectionSnapshot(await invoke("get_connection_snapshot"));
      connectionHydrated = true;
    } catch (error) {
      connectionError = error instanceof Error ? error.message : String(error);
    } finally {
      connectionLoading = false;
      connectionRefreshing = false;
      if (state.tab === "live") renderConnectionInspector();
    }
  }

  function syncLiveRequests() {
    if (streamPaused) return;
    const nextRequests = (Array.isArray(state.recentRequests) ? state.recentRequests : [])
      .filter((request) => Number(request.id) > clearedThroughId)
      .slice(-100);
    const currentIds = new Set(displayedRequests.map((request) => String(request.id)));
    const nextIds = nextRequests.map((request) => String(request.id));
    liveStreamChanged ||= nextIds.length !== displayedRequests.length || nextIds.some((id, index) => id !== String(displayedRequests[index]?.id));
    if (!liveTailFollowing) unseenLiveRequests += nextIds.filter((id) => !currentIds.has(id)).length;
    displayedRequests = nextRequests;
  }

  function renderRequestRow(request, isNew = false) {
    const status = Number(request.status) || 0;
    const fallback = request.result === "fallback";
    const recovering = request.failurePhase === "hybridIdle";
    const releaseRebuild = recovering && status === 1012 && request.failureReason === "service restarting";
    const failed = request.result === "error" && !recovering;
    const route = REQUEST_ROUTE_LABELS[request.route];
    const protocol = route ?? request.transport;
    const networkIssue = isNetworkIssue(request);
    const transport = networkIssue ? `${protocol} · 网络异常` : releaseRebuild ? "Hybrid WS · 发布重建" : recovering ? `${protocol} · 连接恢复` : failed ? `${protocol} · 失败` : route ?? (fallback ? `${request.transport} · 回退` : request.transport);
    const detail = recovering && request.failureReason ? ` title="${escapeHtml(request.failureReason)}"` : "";
    const tooltipId = `network-error-${String(request.id).replace(/[^a-zA-Z0-9_-]/g, "-")}`;
    const networkMarkup = networkIssue
      ? `<span class="c-transport__protocol">${escapeHtml(protocol)}</span><span aria-hidden="true"> · </span><span class="c-transport__network" tabindex="0" aria-describedby="${tooltipId}">网络异常</span><span class="c-transport__tooltip" id="${tooltipId}" role="tooltip">${escapeHtml(NETWORK_ERROR_MESSAGE).replaceAll("\n", "<br>")}</span>`
      : escapeHtml(transport);
    return `<tr class="c-request-row${isNew ? " is-new" : ""}" data-request-id="${escapeHtml(request.id)}"><td>${telemetry.formatClock(request.timestampMs)}</td><td><span class="c-request-status c-request-status--${status < 400 && !failed ? "success" : "error"}">${numberFormatter.format(status)}</span></td><td><code>${escapeHtml(request.path)}</code></td><td><strong>${telemetry.formatBytes(request.rawBytes)}</strong><span aria-hidden="true">→</span><strong>${telemetry.formatBytes(request.sentBytes)}</strong></td><td><span class="c-transport${fallback || recovering ? " c-transport--fallback" : failed ? " c-transport--error" : ""}"${detail}>${networkMarkup}</span></td><td>${telemetry.formatRate(request.rawBytes, request.sentBytes)}</td></tr>`;
  }

  function renderLiveFollow() {
    const control = $("[data-live-follow]");
    if (control) control.hidden = liveTailFollowing;
    const label = $("[data-live-follow-label]");
    if (label) label.textContent = unseenLiveRequests ? `${numberFormatter.format(unseenLiveRequests)} 条新请求` : "回到最新";
  }

  function followLiveTail() {
    liveTailFollowing = true;
    unseenLiveRequests = 0;
    const terminal = $(".c-terminal__window");
    if (terminal) terminal.scrollTop = terminal.scrollHeight;
    renderLiveFollow();
  }

  function renderLiveStream({ animateNew = true } = {}) {
    const body = $("[data-request-stream]");
    if (body) {
      const nextIds = displayedRequests.map((request) => String(request.id));
      if (!liveStreamHydrated) {
        body.innerHTML = displayedRequests.map((request) => renderRequestRow(request)).join("");
        liveStreamHydrated = true;
        renderedRequestIds = nextIds;
      } else if (nextIds.length !== renderedRequestIds.length || nextIds.some((id, index) => id !== renderedRequestIds[index])) {
        let overlap = Math.min(renderedRequestIds.length, nextIds.length);
        while (overlap && renderedRequestIds.slice(-overlap).some((id, index) => id !== nextIds[index])) overlap -= 1;
        const newRequests = displayedRequests.slice(overlap);
        if (!overlap && renderedRequestIds.length) {
          body.innerHTML = displayedRequests.map((request) => renderRequestRow(request)).join("");
        } else {
          const removed = renderedRequestIds.length - overlap;
          for (let index = 0; index < removed; index += 1) body.firstElementChild?.remove();
          if (newRequests.length) body.insertAdjacentHTML("beforeend", newRequests.map((request) => renderRequestRow(request, animateNew)).join(""));
          if (animateNew && newRequests.length) {
            const latest = newRequests.at(-1);
            const rawBytes = Math.max(0, Number(latest.rawBytes) || 0);
            const savedBytes = Math.max(0, rawBytes - (Number(latest.sentBytes) || 0));
            window.TurboStrands?.pulse(rawBytes ? savedBytes / rawBytes : 0);
          }
        }
        renderedRequestIds = nextIds;
      }
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
    if (terminal && !streamPaused && liveTailFollowing && liveStreamChanged) terminal.scrollTop = terminal.scrollHeight;
    liveStreamChanged = false;
    renderLiveFollow();
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

  function applyStatus(status, options = {}) {
    if (status && typeof status === "object") state = { ...state, ...status, technicalDetail: "" };
    syncLiveRequests();
    renderState(options);
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
      state.codexState = "restart_required";
      state.restartRequired = true;
    }
    if (command === "set_autostart") state.autostartEnabled = args.enabled;
    if (command === "set_dock_visible") state.dockVisible = args.visible;
    if (command === "restart_codex") {
      state.desktopRestarted = true;
      state.codexState = "waiting_request";
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

  async function handleAction(action, control) {
    if (action === "toggle-ai-cove-bubble") {
      setAiCoveBubbleOpen(!aiCoveBubbleOpen);
      return;
    }
    if (action === "open-ai-cove") {
      setAiCoveBubbleOpen(false);
      try {
        if (invoke) await invoke("open_ai_cove");
        else window.open?.(AI_COVE_URL, "_blank", "noopener,noreferrer");
      } catch (error) {
        state.configMessage = "无法打开 AI Cove，请在浏览器访问 ai-cove.com。";
        state.technicalDetail = error instanceof Error ? error.message : String(error);
        renderState();
      }
      return;
    }
    if (action === "toggle-connections") {
      if (connectionPanelOpen) {
        setConnectionPanelOpen(false);
      } else {
        setConnectionPanelOpen(true, { trigger: control });
        if (!connectionHydrated) await refreshConnectionSnapshot();
      }
      return;
    }
    if (action === "open-config") {
      selectTab("config", { focus: true });
      return;
    }
    if (action === "follow-live") {
      followLiveTail();
      return;
    }
    if (action === "toggle-stream") {
      streamPaused = !streamPaused;
      if (!streamPaused) syncLiveRequests();
      renderLiveStream({ animateNew: false });
      return;
    }
    if (action === "clear-stream") {
      clearedThroughId = Math.max(clearedThroughId, ...(state.recentRequests ?? []).map((request) => Number(request.id) || 0));
      displayedRequests = [];
      liveTailFollowing = true;
      unseenLiveRequests = 0;
      liveStreamChanged = true;
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
      if (command === "restart_codex" && invoke) {
        try {
          applyStatus(await invoke("get_app_status"));
        } catch {
          state.configMessage = "Codex 重启未完成，请重试。";
        }
      } else {
        state.configMessage = "操作未完成，请按提示重试。";
      }
      state.technicalDetail = error instanceof Error ? error.message : String(error);
    } finally {
      pendingAction = "";
      renderState();
    }
  }

  function canRefreshStatus() {
    return !pendingAction || pendingAction === "install-update";
  }

  async function refreshStatus() {
    if (!invoke || !canRefreshStatus() || refreshing || document.hidden) return;
    refreshing = true;
    try {
      const status = await invoke("get_app_status");
      if (canRefreshStatus()) {
        const animateNew = statusHydrated;
        statusHydrated = true;
        applyStatus(status, { animateNew });
      }
    } catch (error) {
      statusHydrated = true;
      state.serviceHealthy = false;
      state.configMessage = "无法读取 Turbo 状态，请确认应用仍在运行后重试。";
      state.technicalDetail = error instanceof Error ? error.message : String(error);
      renderState();
    } finally {
      refreshing = false;
      if (state.tab === "live") await refreshConnectionSnapshot();
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
    const terminal = $(".c-terminal__window");
    terminal?.addEventListener("scroll", () => {
      liveTailFollowing = terminal.scrollHeight - terminal.scrollTop - terminal.clientHeight <= LIVE_TAIL_THRESHOLD_PX;
      if (liveTailFollowing) unseenLiveRequests = 0;
      renderLiveFollow();
    }, { passive: true });
    document.addEventListener("click", (event) => {
      const action = event.target.closest?.("[data-action]");
      if (action) void handleAction(action.dataset.action, action);
      if (aiCoveBubbleOpen && !event.target.closest?.("[data-ai-cove-popover]")) setAiCoveBubbleOpen(false);
      const tab = event.target.closest?.("[data-tab]");
      if (tab) selectTab(tab.dataset.tab);
    });
    document.addEventListener("pointerover", (event) => {
      const trigger = event.target.closest?.(".c-transport__network");
      if (trigger) positionNetworkTooltip(trigger);
    });
    document.addEventListener("keydown", (event) => {
      if (event.key === "Escape" && aiCoveBubbleOpen) {
        setAiCoveBubbleOpen(false, { restoreFocus: true });
        return;
      }
      if (event.key === "Escape" && connectionPanelOpen) {
        setConnectionPanelOpen(false);
        return;
      }
      const chartSlot = event.target.closest?.(".c-bar-slot");
      if (chartSlot && handleChartKeydown(event, chartSlot)) return;
      const tab = event.target.closest?.("[data-tab]");
      if (tab) handleTabKeydown(event, tab);
    });
    document.addEventListener("focusin", (event) => {
      const networkTrigger = event.target.closest?.(".c-transport__network");
      if (networkTrigger) positionNetworkTooltip(networkTrigger);
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
    document.addEventListener("visibilitychange", () => {
      if (!document.hidden && invoke) void refreshStatus();
    });
    bindConnectionDock();
    bindDotField();
    renderTab();
    renderState();
    renderConnectionInspector();
    updateUrl();
    if (invoke) {
      void refreshStatus();
      window.setInterval(refreshStatus, 1_000);
    }
  }

  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", init, { once: true });
  else init();
})();
