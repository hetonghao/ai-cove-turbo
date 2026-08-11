// PROTOTYPE ONLY — Variant D 验证“绑定会话双列、近期关闭单列并按实际宽度降密度”，不接真实业务数据。
function connectionDensity(count) {
  if (count <= 1) return "full";
  if (count === 2) return "compact";
  return "state-only";
}

if (typeof module !== "undefined") module.exports = { connectionDensity };

if (typeof document !== "undefined") (() => {
  const variants = [
    { key: "A", label: "右侧抽屉" },
    { key: "B", label: "拓扑泳道" },
    { key: "C", label: "右上角检查器" },
    { key: "D", label: "双列会话" },
  ];
  const scenarioLabels = { healthy: "常态", busy: "高负载", parallel: "多 Agent", recovering: "恢复中", flapping: "持续波动" };
  const scenarios = {
    healthy: {
      pool: ["P-01", "P-02", "P-03", "P-04", "P-05", "P-06"].map((id, index) => ({ id, state: "warm", detail: `${18 + index * 3} ms · 已就绪` })),
      threads: [
        { id: "thr_7C2A", state: "active", direction: "down", detail: "正在接收响应", age: "12 KB/s" },
        { id: "thr_91DF", state: "active", direction: "up", detail: "正在发送请求", age: "8 KB/s" },
        { id: "thr_3E08", state: "bound", detail: "已绑定 · 空闲", age: "18 秒", reclaimAfter: 102 },
        { id: "thr_A1B4", state: "bound", detail: "已绑定 · 空闲", age: "41 秒", reclaimAfter: 46 },
      ],
      transitions: [],
      recent: [
        { id: "C-184", reason: "正常关闭 · 1000", time: "12 秒前", abnormal: false },
        { id: "C-179", reason: "容量回收", time: "2 分钟前", abnormal: false },
      ],
      notice: "连接健康。瞬时关闭已自动补位，只保留在近期事件中。",
      severity: "normal",
      fallbacks: 0,
    },
    busy: {
      pool: ["P-01", "P-02"].map((id, index) => ({ id, state: "warm", detail: `${21 + index * 4} ms · 已就绪` })),
      threads: [
        { id: "thr_7C2A", state: "active", direction: "down", detail: "正在接收响应", age: "38 KB/s" },
        { id: "thr_91DF", state: "active", direction: "up", detail: "正在发送请求", age: "14 KB/s" },
        { id: "thr_3E08", state: "active", direction: "down", detail: "正在接收响应", age: "26 KB/s" },
        { id: "thr_A1B4", state: "active", direction: "down", detail: "正在接收响应", age: "11 KB/s" },
        { id: "thr_0C44", state: "active", direction: "up", detail: "正在发送请求", age: "9 KB/s" },
        { id: "thr_6F70", state: "bound", detail: "已绑定 · 空闲", age: "7 秒", reclaimAfter: 113 },
        { id: "thr_C1D2", state: "bound", detail: "已绑定 · 空闲", age: "19 秒", reclaimAfter: 71 },
      ],
      transitions: [
        { id: "N-207", label: "补充预热容量", step: "TLS 握手", elapsed: "680 ms", target: "api.ai-cove.com", attempt: "首次建立" },
        { id: "N-208", label: "新线程绑定连接", step: "连接中", elapsed: "210 ms", target: "thr_B290", attempt: "首次建立" },
      ],
      recent: [{ id: "C-201", reason: "容量回收", time: "34 秒前", abnormal: false }],
      notice: "负载升高，Turbo 正在补充 2 条连接；现有请求未回退。",
      severity: "normal",
      fallbacks: 0,
    },
    parallel: {
      pool: ["P-01", "P-02", "P-03", "P-04"].map((id, index) => ({ id, state: "warm", detail: `${19 + index * 2} ms · 已就绪` })),
      threads: [
        { id: "ws-01-10", sessionId: "thread-01", sessionNo: 1, connectionNo: 10, state: "active", direction: "down", detail: "正在接收响应", age: "18 KB/s" },
        { id: "ws-02-14", sessionId: "thread-02", sessionNo: 2, connectionNo: 14, state: "bound", detail: "已绑定 · 空闲", age: "8 秒", reclaimAfter: 112 },
        { id: "ws-03-02", sessionId: "thread-03", sessionNo: 3, connectionNo: 2, state: "bound", detail: "已绑定 · 空闲", age: "13 秒", reclaimAfter: 107 },
        { id: "ws-04-01", sessionId: "thread-04", sessionNo: 4, connectionNo: 1, state: "active", direction: "up", detail: "正在发送请求", age: "7 KB/s" },
        { id: "ws-05-01", sessionId: "thread-05", sessionNo: 5, connectionNo: 1, state: "bound", detail: "已绑定 · 空闲", age: "24 秒", reclaimAfter: 96 },
        { id: "ws-06-01", sessionId: "thread-06", sessionNo: 6, connectionNo: 1, state: "bound", detail: "已绑定 · 空闲", age: "31 秒", reclaimAfter: 89 },
        { id: "ws-07-01", sessionId: "thread-07", sessionNo: 7, connectionNo: 1, state: "bound", detail: "已绑定 · 空闲", age: "37 秒", reclaimAfter: 83 },
        { id: "ws-08-01", sessionId: "thread-08", sessionNo: 8, connectionNo: 1, state: "active", direction: "down", detail: "正在接收响应", age: "22 KB/s" },
        { id: "ws-08-02", sessionId: "thread-08", sessionNo: 8, connectionNo: 2, state: "bound", detail: "已绑定 · 空闲", age: "5 秒", reclaimAfter: 115 },
        { id: "ws-09-01", sessionId: "thread-09", sessionNo: 9, connectionNo: 1, state: "bound", detail: "已绑定 · 空闲", age: "42 秒", reclaimAfter: 78 },
        { id: "ws-10-01", sessionId: "thread-10", sessionNo: 10, connectionNo: 1, state: "active", direction: "down", detail: "正在接收响应", age: "11 KB/s" },
        { id: "ws-11-01", sessionId: "thread-11", sessionNo: 11, connectionNo: 1, state: "bound", detail: "已绑定 · 空闲", age: "49 秒", reclaimAfter: 71 },
        { id: "ws-12-01", sessionId: "thread-12", sessionNo: 12, connectionNo: 1, state: "active", direction: "up", detail: "正在发送请求", age: "9 KB/s" },
        { id: "ws-13-01", sessionId: "thread-13", sessionNo: 13, connectionNo: 1, state: "bound", detail: "已绑定 · 空闲", age: "54 秒", reclaimAfter: 66 },
        { id: "ws-14-01", sessionId: "thread-14", sessionNo: 14, connectionNo: 1, state: "active", direction: "down", detail: "正在接收响应", age: "29 KB/s" },
        { id: "ws-14-02", sessionId: "thread-14", sessionNo: 14, connectionNo: 2, state: "bound", detail: "已绑定 · 空闲", age: "3 秒", reclaimAfter: 117 },
        { id: "ws-14-03", sessionId: "thread-14", sessionNo: 14, connectionNo: 3, state: "active", direction: "up", detail: "正在发送请求", age: "4 KB/s" },
      ],
      transitions: [],
      recent: [
        { id: "close-15-01", sessionId: "thread-15", sessionNo: 15, connectionNo: 1, reason: "正常关闭 · 1000", time: "8 秒前", abnormal: false },
        { id: "close-15-02", sessionId: "thread-15", sessionNo: 15, connectionNo: 2, reason: "容量回收", time: "15 秒前", abnormal: false },
        { id: "close-15-03", sessionId: "thread-15", sessionNo: 15, connectionNo: 3, reason: "上游关闭 · 1012", time: "21 秒前", abnormal: true },
        { id: "close-15-04", sessionId: "thread-15", sessionNo: 15, connectionNo: 4, reason: "恢复后正常关闭", time: "31 秒前", abnormal: false },
        { id: "close-15-05", sessionId: "thread-15", sessionNo: 15, connectionNo: 5, reason: "线程结束", time: "1 分钟前", abnormal: false },
        { id: "close-16-01", sessionId: "thread-16", sessionNo: 16, connectionNo: 1, reason: "Pong 超时", time: "2 分钟前", abnormal: true },
        { id: "close-17-01", sessionId: "thread-17", sessionNo: 17, connectionNo: 1, reason: "重连替换", time: "2 分钟前", abnormal: false },
        { id: "close-17-02", sessionId: "thread-17", sessionNo: 17, connectionNo: 2, reason: "容量回收", time: "4 分钟前", abnormal: false },
      ],
      notice: "多 Agent 并行运行中；双列布局保持会话可扫读，并压缩多连接标签。",
      severity: "normal",
      fallbacks: 0,
    },
    recovering: {
      pool: [{ id: "P-02", state: "warm", detail: "29 ms · 已就绪" }],
      threads: [
        { id: "thr_7C2A", state: "active", direction: "down", detail: "正在接收响应", age: "6 KB/s" },
        { id: "thr_3E08", state: "bound", detail: "已绑定 · 空闲", age: "4 秒", reclaimAfter: 28 },
      ],
      transitions: [
        { id: "R-211", label: "替换失效连接", step: "重试 1/3", elapsed: "1.4 秒", target: "P-03", attempt: "1012 后首次重试" },
        { id: "R-212", label: "恢复预热池", step: "TLS 握手", elapsed: "920 ms", target: "api.ai-cove.com", attempt: "首次重试" },
        { id: "R-213", label: "恢复预热池", step: "等待连接", elapsed: "350 ms", target: "api.ai-cove.com", attempt: "首次建立" },
      ],
      recent: [
        { id: "C-210", reason: "上游关闭 · 1012", time: "2 秒前", abnormal: true },
        { id: "C-209", reason: "健康检查超时", time: "8 秒前", abnormal: true },
      ],
      notice: "正在自动恢复，已有 1 条请求临时回退到 HTTP。只有超过短暂恢复窗口才显示此提示。",
      severity: "warning",
      fallbacks: 1,
    },
    flapping: {
      pool: [],
      threads: [
        { id: "thr_7C2A", state: "bound", detail: "等待可用连接", age: "3 秒", reclaimAfter: 17 },
        { id: "thr_3E08", state: "bound", detail: "等待可用连接", age: "5 秒", reclaimAfter: 11 },
      ],
      transitions: [
        { id: "R-219", label: "重新建立连接", step: "重试 3/3", elapsed: "8.2 秒", target: "api.ai-cove.com", attempt: "连续 3 次失败" },
        { id: "R-220", label: "重新建立连接", step: "退避 2 秒", elapsed: "1.1 秒", target: "api.ai-cove.com", attempt: "等待下一次重试" },
      ],
      recent: [
        { id: "C-218", reason: "上游关闭 · 1012", time: "1 秒前", abnormal: true },
        { id: "C-217", reason: "Pong 超时", time: "5 秒前", abnormal: true },
        { id: "C-216", reason: "TLS 握手失败", time: "11 秒前", abnormal: true },
        { id: "C-215", reason: "上游关闭 · 1012", time: "19 秒前", abnormal: true },
      ],
      notice: "WebSocket 连续恢复失败，当前无预热连接；请求已回退到 HTTP，建议检查网络或上游状态。",
      severity: "danger",
      fallbacks: 4,
    },
  };

  const query = new URLSearchParams(location.search);
  let variant = variants.some((item) => item.key === query.get("variant")) ? query.get("variant") : "C";
  let scenario = Object.hasOwn(scenarios, query.get("scenario")) ? query.get("scenario") : variant === "D" ? "parallel" : "healthy";
  let scenarioStartedAt = Date.now();
  const layer = document.querySelector("[data-layer]");
  const panel = document.querySelector("[data-panel]");
  const openButton = document.querySelector("[data-open-connections]");

  function icon(state) {
    return `<i class="ws-icon" data-state="${state}" aria-hidden="true"></i>`;
  }

  function shortId(id) {
    return id.replace(/^thr_/, "").replaceAll("-", "");
  }

  function activityGlyph(item) {
    if (item.state !== "active") return `<span class="activity-idle" aria-hidden="true">zzz</span>`;
    const direction = item.direction === "up" ? "up" : "down";
    const path = direction === "up" ? "M6 10V2M3 5l3-3 3 3" : "M6 2v8m3-3-3 3-3-3";
    return `<svg class="activity-glyph" data-direction="${direction}" viewBox="0 0 12 12" aria-hidden="true"><path d="${path}" /></svg>`;
  }

  function closedGlyph() {
    return '<svg class="activity-glyph activity-glyph--closed" viewBox="0 0 12 12" aria-hidden="true"><path d="M3 3l6 6M9 3 3 9" /></svg>';
  }

  function sessionIcon(state) {
    return `<svg class="session-icon" data-state="${state}" viewBox="0 0 14 14" aria-hidden="true"><path d="M3 1.75h8a2 2 0 0 1 2 2v4.5a2 2 0 0 1-2 2H7l-3.25 2v-2H3a2 2 0 0 1-2-2v-4.5a2 2 0 0 1 2-2Z" /></svg>`;
  }

  function formatCountdown(seconds) {
    const value = Math.max(0, seconds);
    const minutes = Math.floor(value / 60);
    const remainder = value % 60;
    return `${String(minutes).padStart(2, "0")}:${String(remainder).padStart(2, "0")}`;
  }

  function refreshCountdowns() {
    const elapsed = Math.floor((Date.now() - scenarioStartedAt) / 1000);
    panel.querySelectorAll("[data-reclaim-after]").forEach((chip) => {
      const remaining = Number(chip.dataset.reclaimAfter) - elapsed;
      const label = `${chip.dataset.tooltipBase} · 预计回收 ${formatCountdown(remaining)}`;
      chip.dataset.tooltip = label;
      chip.setAttribute("aria-label", label);
    });
  }

  function header() {
    return `<header class="panel-header"><div><span class="overline">WEBSOCKET / LIVE POOL</span><h2 id="connection-panel-title">当前连接</h2><p>预热、线程绑定和恢复过程均为本机内存状态</p></div><button class="close-button" type="button" data-close-panel aria-label="关闭">×</button></header>`;
  }

  function controls(data) {
    const active = data.threads.filter((item) => item.state === "active").length;
    const bound = data.threads.filter((item) => item.state === "bound").length;
    return `<div class="scenario-bar" aria-label="模拟状态">${Object.entries(scenarioLabels).map(([key, label]) => `<button type="button" data-scenario="${key}" class="${key === scenario ? "is-active" : ""}">${label}</button>`).join("")}</div>
      <div class="legend"><span>${icon("warm")}空白预热</span><span>${icon("active")}正在传输</span><span>${icon("bound")}绑定线程 · 空闲</span></div>
      <div class="summary-strip"><div class="summary-item"><span>预热可用</span><strong>${data.pool.length}</strong></div><div class="summary-item" data-tone="active"><span>正在传输</span><strong>${active}</strong></div><div class="summary-item" data-tone="bound"><span>绑定线程</span><strong>${bound}</strong></div><div class="summary-item" data-tone="pending"><span>建立 / 恢复</span><strong>${data.transitions.length}</strong></div></div>
      ${data.severity === "normal" ? "" : `<div class="health-banner" data-severity="${data.severity}"><strong>${data.severity === "danger" ? "异常" : "恢复"}</strong><span>${data.notice}</span></div>`}`;
  }

  function poolGroup(data, compact = false) {
    const cards = data.pool.length ? data.pool.map((item) => `<div class="connection-card"><div class="connection-card__top">${icon(item.state)}<strong>${item.id}</strong></div><small>${item.detail}</small></div>`).join("") : `<div class="empty-row">当前没有可用预热连接</div>`;
    return `<section class="group"><div class="group-heading"><h3>预热池</h3><span>${data.pool.length} 条空白连接</span></div><div class="${compact ? "compact-pool" : "connection-grid"}">${cards}</div></section>`;
  }

  function threadGroup(data) {
    const rows = data.threads.map((item) => `<div class="thread-row"><div class="thread-row__title">${icon(item.state)}<strong>${item.id}</strong></div><div class="thread-row__meta"><b>${item.detail}</b><span>${item.age}</span></div></div>`).join("");
    return `<section class="group"><div class="group-heading"><h3>绑定线程</h3><span>${data.threads.length} 条连接</span></div><div class="thread-list">${rows || `<div class="empty-row">暂无绑定线程</div>`}</div></section>`;
  }

  function transitionGroup(data) {
    const rows = data.transitions.map((item) => `<div class="transition-row">${icon("pending")}<strong>${item.id} · ${item.label}</strong><span>${item.step}</span></div>`).join("");
    return `<section class="group"><div class="group-heading"><h3>建立 / 恢复中</h3><span>${data.transitions.length} 个过程</span></div><div class="transition-list">${rows || `<div class="empty-row">没有正在进行的连接操作</div>`}</div></section>`;
  }

  function eventGroup(data, compact = false) {
    const rows = data.recent.map((item) => `<div class="event-row">${icon("closed")}<div><strong>${item.id}</strong><small>${item.reason}</small></div><small>${item.time}</small></div>`).join("");
    return `<section class="group compact-events"><div class="group-heading"><h3>近期关闭</h3><span>最近 5 分钟</span></div><div class="event-list">${rows}</div></section>`;
  }

  function stateDump(data) {
    const snapshot = {
      scenario,
      current: { warm: data.pool.length, active: data.threads.filter((item) => item.state === "active").length, boundIdle: data.threads.filter((item) => item.state === "bound").length, connectingOrRecovering: data.transitions.length },
      recent: { closed: data.recent.length, httpFallbacks: data.fallbacks },
    };
    return `<details class="prototype-state"><summary>查看原型完整状态</summary><pre>${JSON.stringify(snapshot, null, 2)}</pre></details>`;
  }

  function renderA(data) {
    return `${header()}${controls(data)}${data.severity === "normal" ? `<div class="health-banner"><strong>正常</strong><span>${data.notice}</span></div>` : ""}${poolGroup(data)}${threadGroup(data)}${transitionGroup(data)}${eventGroup(data)}${stateDump(data)}`;
  }

  function renderB(data) {
    return `${header()}${controls(data)}<div class="topology-body"><div class="topology-lane topology-lane--pool">${poolGroup(data)}</div><div class="topology-lane topology-lane--threads">${threadGroup(data)}</div><div class="topology-lane topology-lane--events">${transitionGroup(data)}${eventGroup(data)}</div><div class="flow-note"><span>预热池</span><b>→ 请求领取 →</b><span>绑定线程</span><b>→ 关闭 →</b><span>近期事件</span></div></div>${stateDump(data)}`;
  }

  function compactHeader(data) {
    const current = data.pool.length + data.threads.length;
    return `<header class="panel-header panel-header--compact"><div><span class="overline">WEBSOCKET / LIVE POOL</span><h2 id="connection-panel-title">当前连接 <em>${current}</em></h2></div><label class="prototype-scenario"><span>模拟</span><select data-scenario-select>${Object.entries(scenarioLabels).map(([key, label]) => `<option value="${key}" ${key === scenario ? "selected" : ""}>${label}</option>`).join("")}</select></label><button class="close-button" type="button" data-close-panel aria-label="关闭">×</button></header>`;
  }

  function compactPoolGroup(data) {
    const chips = data.pool.length ? data.pool.map((item) => {
      const label = `${item.id} · ${item.detail}`;
      return `<span class="status-chip" tabindex="0" data-tooltip="${label}" aria-label="${label}">${icon(item.state)}<strong>${shortId(item.id)}</strong></span>`;
    }).join("") : `<span class="mini-empty">无可用连接</span>`;
    return `<section class="mini-group"><header><h3>预热池</h3><span>${data.pool.length}</span></header><div class="status-chip-row">${chips}</div></section>`;
  }

  function compactThreadGroup(data) {
    const chips = data.threads.length ? data.threads.map((item) => {
      const label = `${item.id} · ${item.detail} · ${item.age}`;
      const reclaim = item.state === "bound" ? ` data-tooltip-base="${label}" data-reclaim-after="${item.reclaimAfter}"` : "";
      return `<span class="status-chip status-chip--thread" tabindex="0" data-tooltip="${label}" aria-label="${label}"${reclaim}>${icon(item.state)}<strong>${shortId(item.id)}</strong>${activityGlyph(item)}</span>`;
    }).join("") : `<span class="mini-empty">暂无绑定线程</span>`;
    return `<section class="mini-group"><header><h3>绑定线程</h3><span>${data.threads.length}</span></header><div class="status-chip-row">${chips}</div></section>`;
  }

  function compactTransitionGroup(data) {
    const rows = data.transitions.length ? data.transitions.map((item) => `<details class="transition-detail"><summary>${icon("pending")}<strong>${shortId(item.id)} · ${item.label}</strong><span>${item.step}</span></summary><dl><div><dt>目标</dt><dd>${item.target}</dd></div><div><dt>已用时</dt><dd>${item.elapsed}</dd></div><div><dt>阶段</dt><dd>${item.attempt}</dd></div></dl></details>`).join("") : `<span class="mini-empty">当前没有连接操作</span>`;
    return `<section class="mini-group mini-group--transition"><header><h3>建立 / 恢复中</h3><span>${data.transitions.length}</span></header><div class="transition-details">${rows}</div></section>`;
  }

  function compactEventGroup(data) {
    const chips = data.recent.map((item) => {
      const label = `${item.id} · ${item.reason} · ${item.time}`;
      return `<span class="status-chip" tabindex="0" data-tooltip="${label}" aria-label="${label}">${icon("closed")}<strong>${shortId(item.id)}</strong></span>`;
    }).join("");
    return `<section class="mini-group"><header><h3>近期关闭</h3><span>5 分钟 · ${data.recent.length}</span></header><div class="status-chip-row">${chips}</div></section>`;
  }

  function groupBySession(items) {
    const groups = new Map();
    items.forEach((item, index) => {
      const sessionId = item.sessionId || item.id;
      if (!groups.has(sessionId)) groups.set(sessionId, { sessionId, sessionNo: item.sessionNo || index + 1, items: [] });
      groups.get(sessionId).items.push(item);
    });
    return Array.from(groups.values());
  }

  function sessionConnectionChip(item, count, closed = false) {
    const number = String(item.connectionNo || 1).padStart(2, "0");
    const name = `连接 ${number}`;
    const status = closed ? (item.abnormal ? "closed" : "warm") : item.state;
    const glyph = closed ? closedGlyph() : activityGlyph(item);
    const detail = closed ? `${name} · ${item.reason} · ${item.time}` : `${name} · ${item.detail} · ${item.age}`;
    return `<span class="status-chip session-connection" tabindex="0" data-tooltip="${detail}" aria-label="${detail}">${icon(status)}<strong><span class="connection-prefix">连接 </span><span>${number}</span></strong>${glyph}</span>`;
  }

  function sessionCard(group, closed = false) {
    const items = group.items;
    const number = String(group.sessionNo).padStart(2, "0");
    const name = `会话 ${number}`;
    const active = !closed && items.some((item) => item.state === "active");
    const abnormal = closed && items.some((item) => item.abnormal);
    const status = abnormal ? "closed" : active ? "active" : closed ? "warm" : "bound";
    const detail = closed ? `${name} · ${group.sessionId} · ${items.length} 条关闭记录` : `${name} · ${group.sessionId} · ${items.length} 条连接`;
    const density = closed ? "full" : connectionDensity(items.length);
    return `<article class="session-card${closed ? " session-card--recent" : ""}" data-density="${density}"${closed ? " data-auto-density" : ""}><span class="session-card__identity" tabindex="0" data-tooltip="${detail}" aria-label="${detail}">${sessionIcon(status)}<strong>${name}</strong><small>×${items.length}</small></span><i class="session-card__separator" aria-hidden="true"></i><div class="session-card__connections">${items.map((item) => sessionConnectionChip(item, items.length, closed)).join("")}</div></article>`;
  }

  function sessionGridGroup(data, closed = false) {
    const items = closed ? data.recent : data.threads;
    const groups = groupBySession(items);
    const title = closed ? "近期关闭" : "绑定会话";
    const count = closed ? `${items.length} 条 · ${groups.length} 会话` : `${groups.length} 会话 · ${items.length} 连接`;
    const empty = closed ? "暂无关闭记录" : "暂无绑定会话";
    const layoutClass = closed ? "session-list session-list--recent" : "session-grid";
    return `<section class="mini-group mini-group--sessions"><header><h3>${title}</h3><span>${count}</span></header><div class="${layoutClass}">${groups.length ? groups.map((group) => sessionCard(group, closed)).join("") : `<span class="mini-empty">${empty}</span>`}</div></section>`;
  }

  function fitRecentSessionNames() {
    panel.querySelectorAll("[data-auto-density]").forEach((card) => {
      const connections = card.querySelector(".session-card__connections");
      if (!connections || !connections.clientWidth) return;
      for (const density of ["full", "compact", "state-only"]) {
        card.dataset.density = density;
        const chips = Array.from(connections.children);
        const gap = Number.parseFloat(getComputedStyle(connections).columnGap) || 0;
        const requiredWidth = chips.reduce((total, chip) => {
          const style = getComputedStyle(chip);
          const children = Array.from(chip.children).filter((child) => getComputedStyle(child).display !== "none");
          const contentWidth = children.reduce((width, child) => width + child.getBoundingClientRect().width, 0);
          const innerGap = (Number.parseFloat(style.columnGap) || 0) * Math.max(0, children.length - 1);
          return total + contentWidth + innerGap + Number.parseFloat(style.paddingLeft) + Number.parseFloat(style.paddingRight);
        }, 0) + gap * Math.max(0, chips.length - 1);
        if (requiredWidth <= connections.clientWidth + 0.5) break;
      }
    });
  }

  function renderC(data) {
    const warning = data.severity === "normal" ? "" : `<div class="health-banner health-banner--compact" data-severity="${data.severity}"><strong>${data.severity === "danger" ? "异常" : "恢复"}</strong><span>${data.notice}</span></div>`;
    return `${compactHeader(data)}<div class="legend legend--compact"><span>${icon("warm")}预热</span><span>${icon("active")}传输</span><span>${icon("bound")}空闲绑定</span></div><div class="compact-body">${compactPoolGroup(data)}${compactThreadGroup(data)}${compactTransitionGroup(data)}${compactEventGroup(data)}</div>${warning}${stateDump(data)}`;
  }

  function renderD(data) {
    const warning = data.severity === "normal" ? "" : `<div class="health-banner health-banner--compact" data-severity="${data.severity}"><strong>${data.severity === "danger" ? "异常" : "恢复"}</strong><span>${data.notice}</span></div>`;
    return `${compactHeader(data)}<div class="legend legend--compact"><span>${icon("warm")}预热</span><span>${icon("active")}传输</span><span>${icon("bound")}空闲绑定</span></div><div class="compact-body">${compactPoolGroup(data)}${sessionGridGroup(data)}${compactTransitionGroup(data)}${sessionGridGroup(data, true)}</div>${warning}${stateDump(data)}`;
  }

  function syncUrl() {
    const next = new URL(location.href);
    next.searchParams.set("variant", variant);
    next.searchParams.set("scenario", scenario);
    history.replaceState(null, "", next);
  }

  function render() {
    const data = scenarios[scenario];
    panel.dataset.variant = variant;
    layer.dataset.variant = variant;
    panel.setAttribute("aria-modal", String(!["C", "D"].includes(variant)));
    panel.innerHTML = variant === "A" ? renderA(data) : variant === "B" ? renderB(data) : variant === "D" ? renderD(data) : renderC(data);
    if (variant === "D") requestAnimationFrame(fitRecentSessionNames);
    document.querySelector("[data-variant-label]").textContent = `${variant} — ${variants.find((item) => item.key === variant).label}`;
    document.querySelector("[data-live-total]").textContent = String(data.pool.length + data.threads.length + data.transitions.length);
    refreshCountdowns();
    syncUrl();
  }

  function openPanel() {
    if (!layer.hidden && ["C", "D"].includes(variant)) {
      closePanel();
      return;
    }
    layer.hidden = false;
    openButton.setAttribute("aria-expanded", "true");
    render();
    panel.querySelector("[data-close-panel]")?.focus();
  }

  function closePanel() {
    layer.hidden = true;
    openButton.setAttribute("aria-expanded", "false");
    openButton.focus();
  }

  function cycle(direction) {
    const index = variants.findIndex((item) => item.key === variant);
    variant = variants[(index + direction + variants.length) % variants.length].key;
    render();
  }

  openButton.addEventListener("click", openPanel);
  document.querySelector("[data-close-connections]").addEventListener("click", closePanel);
  document.querySelector("[data-previous]").addEventListener("click", () => cycle(-1));
  document.querySelector("[data-next]").addEventListener("click", () => cycle(1));
  panel.addEventListener("click", (event) => {
    if (event.target.closest("[data-close-panel]")) closePanel();
    const scenarioButton = event.target.closest("[data-scenario]");
    if (scenarioButton) {
      scenario = scenarioButton.dataset.scenario;
      scenarioStartedAt = Date.now();
      render();
    }
  });
  panel.addEventListener("change", (event) => {
    if (event.target.matches("[data-scenario-select]")) {
      scenario = event.target.value;
      scenarioStartedAt = Date.now();
      render();
    }
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape" && !layer.hidden) closePanel();
    if (["INPUT", "TEXTAREA"].includes(document.activeElement?.tagName) || document.activeElement?.isContentEditable) return;
    if (event.key === "ArrowLeft") cycle(-1);
    if (event.key === "ArrowRight") cycle(1);
  });
  window.addEventListener("resize", fitRecentSessionNames);

  render();
  setInterval(refreshCountdowns, 1000);
})();
