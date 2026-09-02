(() => {
  "use strict";

  const VARIANTS = [
    { id: "A", name: "紧凑列表" },
    { id: "B", name: "表格 + 详情" },
    { id: "C", name: "命令台阶" },
  ];

  const models = [
    { slug: "gpt-5.3-codex", name: "GPT-5.3 Codex", context: "272k / 872k", reasoning: "low · high", transport: "auto", capability: "WS 可用", visible: true, description: "默认的 Codex 主力候选" },
    { slug: "gpt-5.4", name: "GPT-5.4", context: "272k / 872k", reasoning: "medium · high", transport: "auto", capability: "WS 可用", visible: true, description: "适合复杂分析与长上下文" },
    { slug: "ox-alpha", name: "ox-alpha", context: "125k / 250k", reasoning: "none", transport: "http", capability: "仅 HTTP", visible: true, description: "上游不提供 WebSocket" },
    { slug: "grok-4.6", name: "grok-4.6", context: "128k / 256k", reasoning: "medium", transport: "auto", capability: "WS 可用", visible: false, description: "备用创意与检索候选" },
    { slug: "codex-auto-review", name: "codex-auto-review", context: "125k / 250k", reasoning: "high", transport: "auto", capability: "WS 可用", visible: false, description: "用于代码审查的低频候选" },
  ];

  const state = {
    variant: new URL(window.location.href).searchParams.get("variant")?.toUpperCase() || "A",
    selectedSlug: models[0].slug,
    message: "原型状态：可切换 A / B / C，对比一行密度。",
  };
  if (!VARIANTS.some(({ id }) => id === state.variant)) state.variant = "A";

  const $ = (selector) => document.querySelector(selector);
  const escapeHtml = (value) => String(value).replaceAll("&", "&amp;").replaceAll("<", "&lt;").replaceAll(">", "&gt;").replaceAll('"', "&quot;").replaceAll("'", "&#39;");
  const modelBySlug = (slug) => models.find((model) => model.slug === slug);
  const slugIsDuplicate = (model) => model.name.toLowerCase().replaceAll(/[^a-z0-9]/g, "") === model.slug.toLowerCase().replaceAll(/[^a-z0-9]/g, "");
  const nameMarkup = (model) => `<strong>${escapeHtml(model.name)}</strong>${slugIsDuplicate(model) ? "" : `<code>${escapeHtml(model.slug)}</code>`}`;
  const eyeIcon = (visible) => visible
    ? '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M2.5 12s3.5-5 9.5-5 9.5 5 9.5 5-3.5 5-9.5 5-9.5-5-9.5-5Z"/><circle cx="12" cy="12" r="2.5"/></svg>'
    : '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="m3 3 18 18M10.6 6.2A10.7 10.7 0 0 1 12 6c6 0 9.5 6 9.5 6a17.4 17.4 0 0 1-3.1 3.3M6.1 6.8C3.7 8.3 2.5 12 2.5 12s3.5 5 9.5 5c1 0 1.9-.2 2.7-.4"/><path d="M9.9 9.9a3 3 0 0 0 4.2 4.2"/></svg>';
  const trashIcon = '<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16M9 7V4h6v3M7 7l1 13h8l1-13M10 11v5M14 11v5"/></svg>';
  const dragIcon = '<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="8" cy="7" r="1.2"/><circle cx="16" cy="7" r="1.2"/><circle cx="8" cy="12" r="1.2"/><circle cx="16" cy="12" r="1.2"/><circle cx="8" cy="17" r="1.2"/><circle cx="16" cy="17" r="1.2"/></svg>';

  function actionMarkup(model, compact = false) {
    const label = model.visible ? `隐藏 ${model.name}` : `显示 ${model.name}`;
    return `<div class="row-actions${compact ? " row-actions--compact" : ""}">
      <button class="icon-button" type="button" data-action="visibility" data-slug="${escapeHtml(model.slug)}" aria-pressed="${model.visible}" aria-label="${escapeHtml(label)}" title="${escapeHtml(label)}">${eyeIcon(model.visible)}</button>
      <button class="text-button" type="button" data-action="edit" data-slug="${escapeHtml(model.slug)}">编辑</button>
      <button class="icon-button icon-button--danger" type="button" data-action="delete" data-slug="${escapeHtml(model.slug)}" aria-label="删除 ${escapeHtml(model.name)}" title="删除模型">${trashIcon}</button>
    </div>`;
  }

  function rowA(model, index) {
    const transport = model.transport === "http" ? "HTTP" : "自动";
    return `<article class="model-row model-row--a${state.selectedSlug === model.slug ? " is-selected" : ""}" data-slug="${escapeHtml(model.slug)}" tabindex="0">
      <button class="drag-button" type="button" data-action="select" data-slug="${escapeHtml(model.slug)}" aria-label="拖动 ${escapeHtml(model.name)} 调整优先级">${dragIcon}</button>
      <span class="row-rank">${String(index + 1).padStart(2, "0")}</span>
      <div class="model-identity">${nameMarkup(model)}<span class="model-description">${escapeHtml(model.description)}</span></div>
      <div class="model-facts"><span>${escapeHtml(model.context)}</span><span>${escapeHtml(model.reasoning)}</span><span class="capability" data-capability="${model.capability === "WS 可用" ? "good" : "neutral"}">${escapeHtml(model.capability)}</span></div>
      <button class="transport-button" type="button" data-action="transport" data-slug="${escapeHtml(model.slug)}" aria-label="切换 ${escapeHtml(model.name)} 传输方式">${transport}</button>
      ${actionMarkup(model, true)}
    </article>`;
  }

  function variantA() {
    return `<div class="variant variant--a"><div class="variant-note"><span>A / 推荐先看</span><p>信息压成一条水平扫描线，名称只出现一次，操作永远靠右。</p></div><div class="model-list model-list--a" role="list">${models.map(rowA).join("")}</div></div>`;
  }

  function rowB(model, index) {
    return `<article class="table-row${state.selectedSlug === model.slug ? " is-selected" : ""}" data-slug="${escapeHtml(model.slug)}" tabindex="0">
      <span class="table-rank">${String(index + 1).padStart(2, "0")}</span>
      <div class="table-model">${nameMarkup(model)}<small>${escapeHtml(model.description)}</small></div>
      <span class="table-context">${escapeHtml(model.context)}</span>
      <span class="table-reasoning">${escapeHtml(model.reasoning)}</span>
      <span class="table-capability" data-capability="${model.capability === "WS 可用" ? "good" : "neutral"}">${escapeHtml(model.capability)}</span>
      <span class="table-transport">${model.transport === "http" ? "HTTP" : "自动"}</span>
      ${actionMarkup(model, true)}
    </article>`;
  }

  function variantB() {
    const selected = modelBySlug(state.selectedSlug) || models[0];
    return `<div class="variant variant--b"><div class="variant-note"><span>B / 信息台</span><p>左侧是可快速扫读的表格行，右侧只承载当前选中模型的解释。</p></div><div class="split-layout"><div class="table-wrap"><div class="table-head" aria-hidden="true"><span>#</span><span>模型</span><span>上下文</span><span>思考</span><span>能力</span><span>传输</span><span>操作</span></div><div class="model-list model-list--b" role="list">${models.map(rowB).join("")}</div></div><aside class="model-inspector" aria-label="当前模型详情"><span class="inspector-label">SELECTED MODEL</span><div class="inspector-name">${escapeHtml(selected.name)}</div><code>${escapeHtml(selected.slug)}</code><p>${escapeHtml(selected.description)}</p><dl><div><dt>上下文</dt><dd>${escapeHtml(selected.context)}</dd></div><div><dt>推理</dt><dd>${escapeHtml(selected.reasoning)}</dd></div><div><dt>能力</dt><dd>${escapeHtml(selected.capability)}</dd></div></dl><button type="button" class="inspector-edit" data-action="edit" data-slug="${escapeHtml(selected.slug)}">编辑当前模型</button></aside></div></div>`;
  }

  function rowC(model, index) {
    return `<article class="command-row${state.selectedSlug === model.slug ? " is-selected" : ""}" data-slug="${escapeHtml(model.slug)}" tabindex="0"><span class="command-rank">${String(index + 1).padStart(2, "0")}</span><span class="command-marker" aria-hidden="true"></span><div class="command-model">${nameMarkup(model)}</div><span class="command-context">${escapeHtml(model.context)}</span><span class="command-state ${model.visible ? "is-on" : "is-off"}">${model.visible ? "LIST" : "HIDDEN"}</span><span class="command-transport">${model.transport === "http" ? "HTTP" : "AUTO"}</span>${actionMarkup(model, true)}</article>`;
  }

  function variantC() {
    return `<div class="variant variant--c"><div class="variant-note"><span>C / 命令台阶</span><p>优先级变成视觉主轴，隐藏项降噪，删除是最后一个危险动作。</p></div><div class="command-toolbar"><span><b>${models.filter((model) => model.visible).length}</b> 个可见</span><span>按优先级排序</span><button type="button" data-action="create">+ 添加</button></div><div class="model-list model-list--c" role="list">${models.map(rowC).join("")}</div></div>`;
  }

  function render() {
    const variant = VARIANTS.find(({ id }) => id === state.variant) || VARIANTS[0];
    $("[data-variant-mount]").innerHTML = state.variant === "A" ? variantA() : state.variant === "B" ? variantB() : variantC();
    $("[data-current-variant]").textContent = `${variant.id} · ${variant.name}`;
    $("[data-visible-count]").textContent = String(models.filter((model) => model.visible).length);
    $("[data-total-count]").textContent = String(models.length);
    $("[data-catalog-status]").textContent = state.message;
  }

  function setVariant(next) {
    state.variant = VARIANTS[(VARIANTS.findIndex(({ id }) => id === state.variant) + next + VARIANTS.length) % VARIANTS.length].id;
    const url = new URL(window.location.href);
    url.searchParams.set("variant", state.variant);
    window.history.replaceState({}, "", url);
    state.message = `已切换到 ${state.variant} · ${VARIANTS.find(({ id }) => id === state.variant).name}。继续比较一行里能否读到名称、状态和操作。`;
    render();
  }

  document.addEventListener("click", (event) => {
    const switcher = event.target.closest("[data-switch]");
    if (switcher) { setVariant(switcher.dataset.switch === "prev" ? -1 : 1); return; }
    const control = event.target.closest("[data-action]");
    if (!control) return;
    const model = modelBySlug(control.dataset.slug);
    if (control.dataset.action === "delete" && model) {
      const removedIndex = models.indexOf(model);
      models.splice(removedIndex, 1);
      state.selectedSlug = models[Math.max(0, removedIndex - 1)]?.slug || models[0]?.slug || "";
      state.message = `${model.name} 已从原型目录移除（仅内存演示，可刷新恢复）。`;
      render();
    } else if (control.dataset.action === "visibility" && model) {
      model.visible = !model.visible;
      state.message = `${model.name} 已${model.visible ? "显示给" : "隐藏于"} Codex。`;
      render();
    } else if (control.dataset.action === "transport" && model) {
      model.transport = model.transport === "http" ? "auto" : "http";
      state.message = `${model.name} 的传输方式已切换为 ${model.transport === "http" ? "HTTP" : "自动"}。`;
      render();
    } else if (control.dataset.action === "edit" && model) {
      state.selectedSlug = model.slug;
      state.message = `已选中 ${model.name}，正式页面可在此进入编辑。`;
      render();
    } else if (control.dataset.action === "select" && model) {
      state.selectedSlug = model.slug;
      render();
    } else if (control.dataset.action === "discover") {
      state.message = "原型中保留“从上游发现”入口，未连接真实 Provider。";
      render();
    } else if (control.dataset.action === "create") {
      state.message = "原型中保留“添加模型”入口，未连接真实保存流程。";
      render();
    }
  });

  document.addEventListener("keydown", (event) => {
    if (event.target.matches("input, textarea, select, [contenteditable='true']")) return;
    const row = event.target.closest?.("[data-slug]");
    if (row && (event.key === "Enter" || event.key === " ")) {
      event.preventDefault();
      const model = modelBySlug(row.dataset.slug);
      if (model) {
        state.selectedSlug = model.slug;
        state.message = `已选中 ${model.name}，正式页面可在此进入编辑。`;
        render();
      }
      return;
    }
    if (event.key === "ArrowLeft") { event.preventDefault(); setVariant(-1); }
    if (event.key === "ArrowRight") { event.preventDefault(); setVariant(1); }
  });

  render();
})();
