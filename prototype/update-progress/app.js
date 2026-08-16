(() => {
  const variants = [
    { key: "A", name: "底边进度轨" },
    { key: "B", name: "版本迁移带" },
    { key: "C", name: "分阶段流水线" },
  ];
  const scenarios = {
    available: { label: "发现新版本", message: "v0.1.0-beta.7 可安装", short: "等待安装", progress: 0, stage: -1, tone: "available" },
    download: { label: "下载中", message: "正在下载签名更新", short: "下载更新", progress: 68, stage: 0, tone: "active" },
    verify: { label: "正在校验", message: "正在验证更新签名", short: "签名校验", progress: 88, stage: 1, tone: "active" },
    install: { label: "安装中", message: "正在替换应用文件", short: "安装更新", progress: 96, stage: 2, tone: "active" },
    complete: { label: "更新完成", message: "即将重新启动 Turbo", short: "安装完成", progress: 100, stage: 3, tone: "success" },
    error: { label: "更新失败", message: "网络中断，可重新下载", short: "下载中断", progress: 54, stage: 0, tone: "error" },
  };
  const stageRanges = [[0, 80], [80, 92], [92, 100]];
  const params = new URL(window.location.href).searchParams;
  let variant = variants.some((item) => item.key === params.get("variant")) ? params.get("variant") : "A";
  let scenarioKey = Object.hasOwn(scenarios, params.get("scenario")) ? params.get("scenario") : "download";

  function all(selector) {
    return Array.from(document.querySelectorAll(selector));
  }

  function updateUrl() {
    const url = new URL(window.location.href);
    url.searchParams.set("variant", variant);
    url.searchParams.set("scenario", scenarioKey);
    window.history.replaceState({}, "", url);
  }

  function actionCopy(scenario) {
    if (scenario.tone === "error") return "重新下载";
    if (scenario.tone === "success") return "重新演示";
    if (scenario.stage < 0) return "安装更新";
    return "更新中";
  }

  function render() {
    const scenario = scenarios[scenarioKey];
    document.body.dataset.variant = variant;
    document.body.dataset.scenario = scenarioKey;
    document.body.dataset.tone = scenario.tone;
    document.documentElement.style.setProperty("--progress", String(scenario.progress / 100));

    all("[data-variant-panel]").forEach((panel) => { panel.hidden = panel.dataset.variantPanel !== variant; });
    const currentVariant = variants.find((item) => item.key === variant);
    document.querySelector("[data-variant-label]").textContent = `${currentVariant.key} — ${currentVariant.name}`;
    all("[data-state-label]").forEach((target) => { target.textContent = scenario.label; });
    all("[data-state-message]").forEach((target) => { target.textContent = scenario.message; });
    all("[data-stage-short]").forEach((target) => { target.textContent = scenario.short; });
    all("[data-progress-label]").forEach((target) => { target.textContent = `${scenario.progress}%`; });
    all('[role="progressbar"]').forEach((target) => { target.setAttribute("aria-valuenow", String(scenario.progress)); });

    all("[data-stage]").forEach((target) => {
      const index = Number(target.dataset.stage);
      let stageState = "pending";
      if (scenario.tone === "success" || index < scenario.stage) stageState = "done";
      else if (index === scenario.stage) stageState = scenario.tone === "error" ? "error" : "active";
      target.dataset.stageState = stageState;
      const [start, end] = stageRanges[index];
      const localProgress = Math.max(0, Math.min(100, ((scenario.progress - start) / (end - start)) * 100));
      target.style.setProperty("--stage-progress", String(localProgress / 100));
    });

    all("[data-update-action]").forEach((button) => {
      const busy = scenario.stage >= 0 && scenario.stage < 3 && scenario.tone !== "error";
      button.textContent = actionCopy(scenario);
      button.disabled = busy;
      button.setAttribute("aria-busy", String(busy));
    });
    all("[data-scenario-button]").forEach((button) => {
      button.setAttribute("aria-pressed", String(button.dataset.scenarioButton === scenarioKey));
    });
    updateUrl();
  }

  function stepVariant(direction) {
    const index = variants.findIndex((item) => item.key === variant);
    variant = variants[(index + direction + variants.length) % variants.length].key;
    render();
  }

  all("[data-variant-step]").forEach((button) => {
    button.addEventListener("click", () => stepVariant(Number(button.dataset.variantStep)));
  });
  all("[data-scenario-button]").forEach((button) => {
    button.addEventListener("click", () => {
      scenarioKey = button.dataset.scenarioButton;
      render();
    });
  });
  all("[data-update-action]").forEach((button) => {
    button.addEventListener("click", () => {
      scenarioKey = scenarioKey === "complete" ? "available" : "download";
      render();
    });
  });
  window.addEventListener("keydown", (event) => {
    if (["INPUT", "TEXTAREA"].includes(event.target.tagName) || event.target.isContentEditable) return;
    if (event.key === "ArrowLeft") stepVariant(-1);
    if (event.key === "ArrowRight") stepVariant(1);
  });
  window.addEventListener("popstate", () => {
    const next = new URL(window.location.href).searchParams;
    variant = variants.some((item) => item.key === next.get("variant")) ? next.get("variant") : "A";
    scenarioKey = Object.hasOwn(scenarios, next.get("scenario")) ? next.get("scenario") : "download";
    render();
  });

  render();
})();
