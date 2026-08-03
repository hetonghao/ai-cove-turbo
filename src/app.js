(() => {
  "use strict";

  const TABS = ["config", "runtime"];
  const ACTION_TO_STATE = {
    "toggle-compression": "compression",
    "toggle-autostart": "autostart",
    "restart-codex": "restart",
  };
  const BOOLEAN_STATES = new Set(["compression", "autostart"]);
  const state = {
    tab: "config",
    compression: true,
    autostart: true,
    restart: false,
    requests: 0,
    "raw-bytes": 0,
    "sent-bytes": 0,
  };
  const originalText = new WeakMap();
  const numberFormatter = new Intl.NumberFormat("zh-CN");

  function readTab() {
    const params = new URL(window.location.href).searchParams;
    const requestedTab = params.get("tab");
    if (TABS.includes(requestedTab)) return requestedTab;
    return "config";
  }

  function updateUrl() {
    const url = new URL(window.location.href);
    url.searchParams.delete("variant");
    url.searchParams.set("tab", state.tab);
    window.history.replaceState({}, "", url);
  }

  function remember(target) {
    if (!originalText.has(target)) originalText.set(target, target.textContent.trim());
    return originalText.get(target);
  }

  function formatBytes(bytes) {
    if (bytes >= 1_000_000) return `${(bytes / 1_000_000).toFixed(2)} MB`;
    if (bytes >= 1_000) return `${(bytes / 1_000).toFixed(1)} KB`;
    return `${numberFormatter.format(bytes)} B`;
  }

  function formatState(key, target) {
    if (BOOLEAN_STATES.has(key)) return state[key] ? remember(target) : "关";
    if (key === "restart") return state.restart ? "已请求重启" : remember(target);
    if (key === "requests") return numberFormatter.format(state[key]);
    if (key === "raw-bytes" || key === "sent-bytes") return formatBytes(state[key]);
    if (key === "ratio") {
      if (state["raw-bytes"] === 0) return "—";
      return `${((1 - state["sent-bytes"] / state["raw-bytes"]) * 100).toFixed(1)}%`;
    }
    return String(state[key] ?? "");
  }

  function renderState() {
    document.querySelectorAll("[data-state]").forEach((target) => {
      const key = target.dataset.state;
      target.textContent = formatState(key, target);
    });

    document.querySelectorAll("[data-action]").forEach((control) => {
      const key = ACTION_TO_STATE[control.dataset.action];
      if (BOOLEAN_STATES.has(key)) {
        control.dataset.enabled = String(state[key]);
        control.setAttribute("aria-pressed", String(state[key]));
      } else if (key === "restart") {
        control.dataset.status = state.restart ? "requested" : "idle";
      }
    });
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

  function handleAction(action) {
    const key = ACTION_TO_STATE[action];
    if (!key) return;

    if (key === "restart") {
      state.restart = true;
    } else {
      state[key] = !state[key];
    }
    renderState();
  }

  function handleTabKeydown(event, currentTab) {
    const currentIndex = TABS.indexOf(currentTab.dataset.tab);
    let nextIndex = currentIndex;
    if (event.key === "ArrowRight") nextIndex = (currentIndex + 1) % TABS.length;
    else if (event.key === "ArrowLeft") nextIndex = (currentIndex - 1 + TABS.length) % TABS.length;
    else if (event.key === "Home") nextIndex = 0;
    else if (event.key === "End") nextIndex = TABS.length - 1;
    else return;

    event.preventDefault();
    selectTab(TABS[nextIndex], { focus: true });
  }

  function init() {
    state.tab = readTab();

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

    window.addEventListener("popstate", () => {
      state.tab = readTab();
      renderTab();
    });

    renderTab();
    renderState();
    updateUrl();
  }

  if (document.readyState === "loading") {
    document.addEventListener("DOMContentLoaded", init, { once: true });
  } else {
    init();
  }
})();
