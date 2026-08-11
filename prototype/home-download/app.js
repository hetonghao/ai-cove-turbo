(() => {
  "use strict";

  const VARIANTS = ["A", "B", "C"];
  const VARIANT_LABELS = {
    A: "共享应用行",
    B: "安静展开模块",
    C: "共享基础连接条",
  };
  const PLATFORM_LABELS = {
    macos: "macOS",
    windows: "Windows",
  };
  const panels = [...document.querySelectorAll("[data-variant-panel]")];
  const tabs = [...document.querySelectorAll("[data-variant-switch]")];
  const disclosureToggles = [...document.querySelectorAll("[data-disclosure-toggle]")];
  const liveStatus = document.querySelector("[data-variant-status]");
  let currentVariant = "A";

  function readVariant() {
    const requested = new URL(window.location.href).searchParams.get("variant")?.toUpperCase();
    return VARIANTS.includes(requested) ? requested : "A";
  }

  function updateUrl() {
    const url = new URL(window.location.href);
    url.searchParams.set("variant", currentVariant);
    window.history.replaceState({}, "", url);
  }

  function announce(message) {
    if (liveStatus) liveStatus.textContent = message;
  }

  function resetDisclosures() {
    disclosureToggles.forEach((toggle) => {
      const panel = document.getElementById(toggle.getAttribute("aria-controls"));
      const icon = toggle.querySelector(".disclosure-toggle__icon");
      const hint = toggle.querySelector(".disclosure-toggle__hint");
      toggle.setAttribute("aria-expanded", "false");
      if (panel) {
        panel.hidden = true;
        panel.setAttribute("aria-hidden", "true");
      }
      if (icon) icon.textContent = "+";
      if (hint) hint.textContent = "展开下载";
    });
  }

  function setVariant(value, { focus = false, write = true, announceChange = true } = {}) {
    if (!VARIANTS.includes(value)) return;
    currentVariant = value;
    document.body.dataset.variant = value;

    panels.forEach((panel) => {
      const active = panel.dataset.variantPanel === value;
      panel.hidden = !active;
      panel.setAttribute("aria-hidden", String(!active));
    });

    tabs.forEach((tab) => {
      const active = tab.dataset.variantSwitch === value;
      tab.setAttribute("aria-selected", String(active));
      tab.tabIndex = active ? 0 : -1;
      if (active && focus) tab.focus();
    });

    resetDisclosures();
    if (write) updateUrl();
    if (announceChange) announce(`已切换到 ${value} 方案：${VARIANT_LABELS[value]}`);
  }

  function isEditableTarget(target) {
    return target instanceof HTMLElement && target.matches("input, textarea, select, [contenteditable=\"true\"]");
  }

  function isVariantTabTarget(target) {
    return target instanceof HTMLElement && target.matches("[data-variant-switch]");
  }

  function stepVariant(offset) {
    const index = VARIANTS.indexOf(currentVariant);
    const next = (index + offset + VARIANTS.length) % VARIANTS.length;
    setVariant(VARIANTS[next], { focus: true });
  }

  function detectPlatform() {
    if (typeof navigator === "undefined") return "macos";
    const userAgentDataPlatform = navigator.userAgentData?.platform || "";
    const platform = [userAgentDataPlatform, navigator.platform, navigator.userAgent].join(" ").toLowerCase();
    return platform.includes("win") ? "windows" : "macos";
  }

  function bindHomepageDownloads() {
    const platform = detectPlatform();
    const platformLabel = PLATFORM_LABELS[platform];
    const hrefKey = platform === "windows" ? "windowsHref" : "macosHref";

    document.querySelectorAll("[data-home-download]").forEach((link) => {
      const product = link.dataset.product || "桌面应用";
      const label = link.querySelector("[data-home-download-label]");
      const href = link.dataset[hrefKey];
      link.dataset.platform = platform;
      if (href) link.setAttribute("href", href);
      if (label) label.textContent = `下载 ${product} ${platformLabel} 桌面版`;
      link.setAttribute("aria-label", `下载 ${product} ${platformLabel} 桌面版`);
    });
  }

  function setDownloadFeedback(link, message) {
    const scope = link.closest(".app-disclosure, .connected-entry, .variant-panel--a, .homepage-download-preview");
    const feedback = scope?.querySelector("[data-download-feedback]");
    if (feedback) feedback.textContent = message;
    announce(message);
  }

  function bindDownloads() {
    document.querySelectorAll("[data-download]").forEach((link) => {
      link.addEventListener("click", (event) => {
        event.preventDefault();
        const product = link.dataset.product || "桌面应用";
        const platform = PLATFORM_LABELS[link.dataset.platform] || "桌面";
        const message = `${product} ${platform} 入口已选择；throwaway 原型不执行真实下载，包与更新细节待接入。`;
        setDownloadFeedback(link, message);
      });
    });
  }

  function bindDisclosures() {
    disclosureToggles.forEach((toggle) => {
      toggle.addEventListener("click", () => {
        const panel = document.getElementById(toggle.getAttribute("aria-controls"));
        if (!panel) return;
        const expanded = toggle.getAttribute("aria-expanded") === "true";
        const nextExpanded = !expanded;
        const icon = toggle.querySelector(".disclosure-toggle__icon");
        const hint = toggle.querySelector(".disclosure-toggle__hint");
        const product = toggle.dataset.product || "应用";
        toggle.setAttribute("aria-expanded", String(nextExpanded));
        panel.hidden = !nextExpanded;
        panel.setAttribute("aria-hidden", String(!nextExpanded));
        if (icon) icon.textContent = nextExpanded ? "−" : "+";
        if (hint) hint.textContent = nextExpanded ? "收起下载" : "展开下载";
        announce(nextExpanded ? `已展开 ${product} 桌面入口。` : `已收起 ${product} 桌面入口。`);
      });
    });
  }

  tabs.forEach((tab) => {
    tab.addEventListener("click", () => setVariant(tab.dataset.variantSwitch, { focus: true }));
  });

  document.addEventListener("keydown", (event) => {
    if (event.metaKey || event.ctrlKey || event.altKey || isEditableTarget(event.target)) return;
    if (!isVariantTabTarget(event.target) && !isVariantTabTarget(document.activeElement)) return;
    if (event.key === "ArrowRight") {
      event.preventDefault();
      stepVariant(1);
    } else if (event.key === "ArrowLeft") {
      event.preventDefault();
      stepVariant(-1);
    } else if (event.key === "Home") {
      event.preventDefault();
      setVariant(VARIANTS[0], { focus: true });
    } else if (event.key === "End") {
      event.preventDefault();
      setVariant(VARIANTS.at(-1), { focus: true });
    }
  });

  window.addEventListener("popstate", () => setVariant(readVariant(), { write: false, announceChange: false }));

  bindHomepageDownloads();
  bindDownloads();
  bindDisclosures();
  setVariant(readVariant(), { write: false, announceChange: false });
})();
