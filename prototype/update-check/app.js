(() => {
  "use strict";

  // PROTOTYPE: 只验证更新提示气泡与跳转逻辑，不重做现有更新流程。
  const variants = [
    { key: "A", name: "版本栏锚定气泡" },
    { key: "B", name: "品牌入口下方气泡" },
    { key: "C", name: "右下角轻提示" },
  ];
  const baseDate = new Date();
  const latestVersion = "0.1.0-beta.11";
  const state = {
    variant: "A", dayOffset: 0, route: "turbo", currentVersion: "0.1.0-beta.10", latestVersion,
    lastCheckDay: null, ignoredDay: null, updateState: "idle", bubbleOpen: false,
    checkCount: 0, updateProgress: 0, autoClickStatus: "未触发", activity: [],
  };
  let checkTimer = 0;
  let installTimer = 0;
  let progressTimer = 0;
  const $ = (selector) => document.querySelector(selector);
  const all = (selector) => Array.from(document.querySelectorAll(selector));
  const dayKey = () => {
    const date = new Date(baseDate);
    date.setDate(date.getDate() + state.dayOffset);
    return [date.getFullYear(), date.getMonth() + 1, date.getDate()].map((part, index) => index === 0 ? String(part) : String(part).padStart(2, "0")).join("-");
  };
  const dayLabel = () => (state.dayOffset === 0 ? "今天" : "明天");
  const addActivity = (message) => {
    state.activity.unshift(`${dayKey()} · ${message}`);
    state.activity = state.activity.slice(0, 8);
  };

  function updateUrl() {
    const url = new URL(window.location.href);
    url.searchParams.set("variant", state.variant);
    url.searchParams.set("day", String(state.dayOffset));
    window.history.replaceState({}, "", url);
  }

  function setRoute(route) {
    state.route = route;
    document.body.dataset.route = route;
    all("[data-route-view]").forEach((view) => { view.hidden = view.dataset.routeView !== route; });
    const existingUpdateButton = $(".existing-update-button");
    if (existingUpdateButton && route === "turbo") existingUpdateButton.disabled = true;
    const view = $(`[data-route-view="${route}"]`);
    if (view && window.gsap && route === "update-page") window.gsap.fromTo(view, { opacity: 0.35, y: 8 }, { opacity: 1, y: 0, duration: 0.28, ease: "power2.out" });
  }

  function setVariant(variant) {
    const changed = state.variant !== document.body.dataset.variant;
    state.variant = variant;
    document.body.dataset.variant = variant;
    all("[data-variant-panel]").forEach((panel) => { panel.hidden = panel.dataset.variantPanel !== variant; });
    const selected = variants.find((item) => item.key === variant);
    $("[data-variant-label]").textContent = `${selected.key} — ${selected.name}`;
    const panel = $(`[data-variant-panel="${variant}"]`);
    if (changed && panel && window.gsap && variant !== "C") window.gsap.fromTo(panel, { opacity: 0.35, x: 10 }, { opacity: 1, x: 0, duration: 0.24, ease: "power2.out" });
  }

  function setStatus(updateState, message, bubble = state.bubbleOpen) {
    state.updateState = updateState;
    state.bubbleOpen = bubble;
    all("[data-card-state]").forEach((target) => { target.textContent = updateState === "available" ? "发现新版本" : message; });
    all("[data-card-message]").forEach((target) => { target.textContent = message; });
    all("[data-update-card]").forEach((card) => { card.hidden = !bubble; });
  }

  function openTurbo() {
    clearTimeout(checkTimer);
    setRoute("turbo");
    const today = dayKey();
    if (state.lastCheckDay === today) {
      if (state.ignoredDay === today) setStatus("ignored", "今天已忽略，明天再次提醒", false);
      else if (state.updateState === "available") setStatus("available", `v${latestVersion} 可以安装`, true);
      render();
      return;
    }
    state.lastCheckDay = today;
    state.checkCount += 1;
    state.updateProgress = 0;
    setStatus("checking", "正在检查签名更新", false);
    addActivity(`开始第 ${state.checkCount} 次每日检查`);
    render();
    checkTimer = window.setTimeout(() => {
      const available = state.currentVersion !== latestVersion;
      setStatus(available ? "available" : "current", available ? `v${latestVersion} 可以安装` : "Turbo 已是最新", available);
      addActivity(available ? `发现 v${latestVersion}` : "确认已经是最新版本");
      render();
    }, 600);
  }

  function ignoreUpdate() {
    if (state.updateState !== "available") return;
    state.ignoredDay = dayKey();
    setStatus("ignored", "今天已忽略，明天再次提醒", false);
    addActivity("忽略本次更新提醒");
    render();
  }

  function installUpdate() {
    if (state.updateState !== "available") return;
    clearTimeout(installTimer);
    clearInterval(progressTimer);
    state.bubbleOpen = false;
    state.updateState = "redirecting";
    state.autoClickStatus = "正在进入更新页";
    state.updateProgress = 0;
    addActivity("选择立即更新，跳转到现有更新页");
    setRoute("update-page");
    render();
    installTimer = window.setTimeout(() => {
      const existingUpdateButton = $(".existing-update-button");
      if (existingUpdateButton) {
        existingUpdateButton.disabled = false;
        existingUpdateButton.click();
      }
      let progress = 0;
      progressTimer = window.setInterval(() => {
        progress = Math.min(100, progress + 25);
        state.updateProgress = progress;
        if (progress >= 100) {
          clearInterval(progressTimer);
          state.updateState = "updated";
          state.currentVersion = latestVersion;
          state.autoClickStatus = "已完成，现有流程接管";
          addActivity(`现有流程完成安装 v${latestVersion}`);
        }
        render();
      }, 360);
    }, 480);
  }

  function switchDay(offset) {
    state.dayOffset = offset;
    state.autoClickStatus = "未触发";
    addActivity(offset === 0 ? "切回今天" : "模拟进入明天");
    openTurbo();
  }

  function reset() {
    clearTimeout(checkTimer);
    clearTimeout(installTimer);
    clearInterval(progressTimer);
    Object.assign(state, { dayOffset: 0, route: "turbo", currentVersion: "0.1.0-beta.10", lastCheckDay: null, ignoredDay: null, updateState: "idle", bubbleOpen: false, checkCount: 0, updateProgress: 0, autoClickStatus: "未触发", activity: [] });
    const existingUpdateButton = $(".existing-update-button");
    if (existingUpdateButton) existingUpdateButton.disabled = true;
    addActivity("重置原型状态");
    openTurbo();
  }

  function render() {
    const currentDay = dayKey();
    const labels = { idle: "等待检查", checking: "检查中", available: "发现新版本", ignored: "已忽略", current: "已是最新", redirecting: "正在跳转", installing: "安装中", updated: "已完成" };
    const stateLabel = labels[state.updateState] || state.updateState;
    $("[data-hero-status]").textContent = state.updateState === "available" ? `v${latestVersion} 可更新` : stateLabel;
    $("[data-status-dot]").dataset.state = state.updateState;
    $("[data-route-label]").textContent = state.route === "update-page" ? "已跳转到现有更新页" : "已返回现有配置页";
    $("[data-auto-click-status]").textContent = state.autoClickStatus;
    $("[data-install-status]").textContent = state.updateState === "installing" ? "现有安装流程进行中" : state.updateState === "updated" ? "现有流程完成" : "准备交给现有流程";
    $("[data-install-progress]").style.transform = `scaleX(${state.updateProgress / 100})`;
    $('[data-route-view="update-page"] [role="progressbar"]').setAttribute("aria-valuenow", String(state.updateProgress));
    $("[data-footer-state]").textContent = state.activity[0] || `${dayLabel()}等待每日检查`;
    $("[data-state-inspector]").textContent = JSON.stringify({ ...state, day: currentDay }, null, 2);
    all('[data-action="install"], [data-action="ignore"]').forEach((control) => { control.disabled = state.updateState !== "available"; });
    setVariant(state.variant);
    setRoute(state.route);
    updateUrl();
  }

  function handleAction(action) {
    if (action === "ignore") ignoreUpdate();
    if (action === "install") installUpdate();
    if (action === "open") openTurbo();
    if (action === "reset") reset();
    if (action === "day-today") switchDay(0);
    if (action === "day-tomorrow") switchDay(1);
  }

  function handleExistingUpdateClick() {
    if (state.route !== "update-page") return;
    state.autoClickStatus = "已自动点击更新";
    state.updateState = "installing";
    addActivity("现有更新页收到自动点击");
    render();
  }

  function stepVariant(direction) {
    const index = variants.findIndex((item) => item.key === state.variant);
    setVariant(variants[(index + direction + variants.length) % variants.length].key);
    render();
  }

  document.addEventListener("click", (event) => {
    const action = event.target.closest?.("[data-action]")?.dataset.action;
    if (action) handleAction(action);
    const direction = event.target.closest?.("[data-variant-step]")?.dataset.variantStep;
    if (direction) stepVariant(Number(direction));
  });
  $(".existing-update-button").addEventListener("click", handleExistingUpdateClick);
  window.addEventListener("keydown", (event) => {
    if (["INPUT", "TEXTAREA"].includes(event.target.tagName) || event.target.isContentEditable) return;
    if (event.key === "ArrowLeft") stepVariant(-1);
    if (event.key === "ArrowRight") stepVariant(1);
  });
  window.addEventListener("popstate", () => { const params = new URL(window.location.href).searchParams; const requested = params.get("variant"); if (variants.some((item) => item.key === requested)) state.variant = requested; state.dayOffset = Number(params.get("day")) === 1 ? 1 : 0; render(); });

  const params = new URL(window.location.href).searchParams;
  if (variants.some((item) => item.key === params.get("variant"))) state.variant = params.get("variant");
  state.dayOffset = Number(params.get("day")) === 1 ? 1 : 0;
  render();
  openTurbo();
})();
