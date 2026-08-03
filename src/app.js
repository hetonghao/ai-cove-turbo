(() => {
  "use strict";

  const TABS = ["config", "runtime"];
  const invoke = window.__TAURI__?.core?.invoke;
  const numberFormatter = new Intl.NumberFormat("zh-CN");
  const desktopStatus = {
    serviceHealthy: false,
    endpoint: "—",
    configState: "starting",
    configMessage: "正在读取 Turbo 状态",
    provider: "—",
    upstream: "—",
    aiCoveUpstream: true,
    aiCoveUpstreamFixAvailable: false,
    compressionEnabled: true,
    compressionVerified: false,
    websocketEnabled: true,
    websocketVerified: false,
    websocketState: "waiting",
    websocketHandshakes: 0,
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
    updateState: "idle",
    updateMessage: "尚未检查更新",
    updateProgress: 0,
  };
  const previewStatus = {
    ...desktopStatus,
    serviceHealthy: true,
    endpoint: "http://127.0.0.1:44175/v1",
    configState: "active",
    configMessage: "Preview：配置已接管",
    provider: "ai-cove",
    upstream: "https://api.ai-cove.com/v1",
    websocketVerified: true,
    websocketState: "connected",
    websocketHandshakes: 8,
    httpFallbacks: 2,
    requests: 24,
    rawBytes: 1_840_000,
    sentBytes: 1_060_000,
    compressionRatio: 42.4,
    updateMessage: "Preview：尚未检查更新",
  };
  let state = { ...(invoke ? desktopStatus : previewStatus), tab: "config", nonAiCoveConfirmed: false };
  let pendingAction = "";
  let refreshing = false;

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
    return TABS.includes(requestedTab) ? requestedTab : "config";
  }

  function updateUrl() {
    const url = new URL(window.location.href);
    url.searchParams.delete("variant");
    url.searchParams.set("tab", state.tab);
    window.history.replaceState({}, "", url);
  }

  function formatBytes(value) {
    const bytes = Number(value) || 0;
    if (bytes >= 1_000_000) return `${(bytes / 1_000_000).toFixed(2)} MB`;
    if (bytes >= 1_000) return `${(bytes / 1_000).toFixed(1)} KB`;
    return `${numberFormatter.format(bytes)} B`;
  }

  function formatConfigState() {
    const labels = {
      active: "已接管",
      healthy: "已接管",
      managed: "已接管",
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

  function formatVerification() {
    if (!state.compressionEnabled) return "已关闭";
    return state.compressionVerified ? "已验证 zstd" : "等待真实请求验证";
  }

  function formatWebsocketStatus() {
    if (!state.websocketEnabled) return "已关闭";
    const labels = {
      connected: "已连接",
      closed: "已验证 · 当前已关闭",
      failed: "握手失败 · 等待 HTTP 回退",
      conflict: "配置被外部修改",
      waiting: "等待首次握手验证",
    };
    return labels[String(state.websocketState).toLowerCase()] ?? "等待首次握手验证";
  }

  function formatWebsocketDetail() {
    if (!state.websocketEnabled) return "未启用";
    const websocketState = String(state.websocketState).toLowerCase();
    if (websocketState === "failed") return "握手失败";
    if (websocketState === "conflict") return "配置冲突";
    return "扩展由上游协商";
  }

  function formatUpdateState() {
    const labels = {
      idle: "尚未检查",
      checking: "检查中",
      current: "已是最新",
      available: "发现新版本",
      downloading: "下载中",
      installing: "安装中",
      ready: "安装完成",
      unconfigured: "未配置",
      error: "更新失败",
    };
    return labels[String(state.updateState).toLowerCase()] ?? state.updateState ?? "未知";
  }

  function formatState(key) {
    const configState = String(state.configState ?? "unknown").toUpperCase();
    const starting = configState === "STARTING";
    const runtimeMode = invoke ? "DESKTOP" : "PREVIEW";
    const values = {
      "runtime-mode": runtimeMode,
      "service-label": `${invoke ? "AI Cove" : "PREVIEW"} / ${starting ? "正在读取状态" : state.serviceHealthy ? "本地服务正常" : "本地服务异常"}`,
      "service-title": starting ? "正在读取状态" : state.serviceHealthy ? "通道运行中" : "通道未就绪",
      "health-symbol": starting ? "…" : state.serviceHealthy ? "✓" : "!",
      endpoint: state.endpoint || "—",
      "config-state": formatConfigState(),
      "config-message": state.configMessage || "—",
      provider: state.provider || "—",
      upstream: state.upstream || "—",
      "compression-verified": formatVerification(),
      compression: state.compressionEnabled ? "开" : "关",
      websocket: state.websocketEnabled ? "开" : "关",
      "websocket-status": formatWebsocketStatus(),
      "websocket-detail": formatWebsocketDetail(),
      "websocket-handshakes": numberFormatter.format(Number(state.websocketHandshakes) || 0),
      "http-fallbacks": numberFormatter.format(Number(state.httpFallbacks) || 0),
      autostart: state.autostartEnabled ? "开" : "关",
      dock: state.dockVisible ? "开" : "关",
      restart: pendingAction === "restart-codex"
        ? "正在重启…"
        : state.desktopRestarted
          ? "已重启 Codex 桌面端"
          : state.restartRequired
            ? "需要重启 Codex 桌面端"
            : "重启 Codex 桌面端",
      requests: numberFormatter.format(Number(state.requests) || 0),
      "raw-bytes": formatBytes(state.rawBytes),
      "sent-bytes": formatBytes(state.sentBytes),
      ratio: Number(state.rawBytes) > 0 && Number.isFinite(Number(state.compressionRatio)) ? `${Number(state.compressionRatio).toFixed(1)}%` : "—",
      "update-state": formatUpdateState(),
      "update-message": state.updateMessage || "—",
      "update-progress": `${Math.max(0, Math.min(100, Number(state.updateProgress) || 0))}%`,
      "service-runtime": starting ? "STARTING" : state.serviceHealthy ? "HEALTHY" : "OFFLINE",
      "config-runtime": ["ACTIVE", "HEALTHY", "MANAGED"].includes(configState) ? "READY" : configState,
      "verify-runtime": !state.compressionEnabled ? "OFF" : state.compressionVerified ? "VERIFIED" : "WAITING",
      "websocket-runtime": !state.websocketEnabled
        ? "DISABLED"
        : state.websocketVerified
          ? String(state.websocketState).toUpperCase()
          : String(state.websocketState || "waiting").toUpperCase(),
      "observed-state": Number(state.requests) > 0 || Number(state.websocketHandshakes) > 0 ? "OBSERVED / LIVE" : "OBSERVED / WAITING",
      "stream-state": starting ? "WAITING" : state.serviceHealthy ? (Number(state.requests) > 0 || state.websocketState === "connected" ? "ACTIVE" : "IDLE") : "OFFLINE",
    };
    return String(values[key] ?? "");
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
    document.querySelectorAll("[data-visible]").forEach((target) => {
      target.hidden = !visible[target.dataset.visible];
    });
  }

  function renderControls() {
    const pressed = {
      "toggle-compression": state.compressionEnabled,
      "toggle-websocket": state.websocketEnabled,
      "toggle-autostart": state.autostartEnabled,
      "toggle-dock": state.dockVisible,
    };
    document.querySelectorAll("[data-action]").forEach((control) => {
      const action = control.dataset.action;
      control.disabled = Boolean(pendingAction);
      control.dataset.status = pendingAction === action ? "pending" : "idle";
      control.setAttribute("aria-busy", String(pendingAction === action));
      if (Object.hasOwn(pressed, action)) {
        control.dataset.enabled = String(pressed[action]);
        control.setAttribute("aria-pressed", String(pressed[action]));
      }
    });
  }

  function renderVolumes() {
    const rawBytes = Number(state.rawBytes) || 0;
    const sentBytes = Number(state.sentBytes) || 0;
    const values = { raw: rawBytes > 0 ? 100 : 0, sent: rawBytes > 0 ? Math.min(100, sentBytes / rawBytes * 100) : 0 };
    document.querySelectorAll("[data-volume]").forEach((bar) => {
      bar.style.setProperty("--volume", `${values[bar.dataset.volume]}%`);
    });
  }

  function renderState() {
    document.body.dataset.serviceHealthy = String(Boolean(state.serviceHealthy));
    document.querySelectorAll("[data-state]").forEach((target) => {
      target.textContent = formatState(target.dataset.state);
    });
    document.querySelectorAll("[data-state-progress]").forEach((target) => {
      target.style.setProperty("--progress", `${Math.max(0, Math.min(100, Number(state.updateProgress) || 0))}%`);
    });
    document.querySelectorAll('[role="progressbar"]').forEach((target) => {
      target.setAttribute("aria-valuenow", String(Math.max(0, Math.min(100, Number(state.updateProgress) || 0))));
    });
    document.querySelectorAll('[data-state="health-symbol"]').forEach((target) => {
      target.setAttribute("aria-label", String(state.configState).toLowerCase() === "starting" ? "正在读取状态" : state.serviceHealthy ? "本地服务正常" : "本地服务异常");
    });
    renderVisibility();
    renderControls();
    renderVolumes();
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

  function applyStatus(status) {
    if (status && typeof status === "object") state = { ...state, ...status };
    renderState();
  }

  function applyPreviewAction(command, args) {
    if (command === "set_compression") state.compressionEnabled = args.enabled;
    if (command === "set_websocket") {
      state.websocketEnabled = args.enabled;
      state.websocketVerified = false;
      state.websocketState = args.enabled ? "waiting" : "disabled";
      state.restartRequired = true;
    }
    if (command === "set_autostart") state.autostartEnabled = args.enabled;
    if (command === "set_dock_visible") state.dockVisible = args.visible;
    if (command === "restart_codex") {
      state.desktopRestarted = true;
      state.restartRequired = false;
    }
    if (command === "retry_takeover") state.configState = "active";
    if (command === "set_ai_cove_upstream") {
      state.aiCoveUpstream = true;
      state.aiCoveUpstreamFixAvailable = false;
      state.upstream = "https://api.ai-cove.com/v1";
      state.configState = "active";
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
      state.configMessage = `操作失败：${error instanceof Error ? error.message : String(error)}`;
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
      state.configMessage = `状态读取失败：${error instanceof Error ? error.message : String(error)}`;
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

  function init() {
    state.tab = readTab();
    document.addEventListener("click", (event) => {
      const action = event.target.closest?.("[data-action]");
      if (action) void handleAction(action.dataset.action);
      const tab = event.target.closest?.("[data-tab]");
      if (tab) selectTab(tab.dataset.tab);
    });
    document.addEventListener("keydown", (event) => {
      const tab = event.target.closest?.("[data-tab]");
      if (tab) handleTabKeydown(event, tab);
    });
    window.addEventListener("popstate", () => selectTab(readTab(), { updateUrl: false }));
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
