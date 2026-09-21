import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import vm from "node:vm";

class MiniElement {
  constructor(tag, ownerDocument) {
    this.tagName = tag.toUpperCase();
    this.ownerDocument = ownerDocument;
    this.children = [];
    this.parentNode = null;
    this.dataset = {};
    this.attributes = new Map();
    this.listeners = new Map();
    this.hidden = false;
    this.disabled = false;
    this.open = false;
    this._text = "";
    this.focused = false;
  }
  get textContent() { return this._text; }
  set textContent(value) {
    this._text = String(value ?? "");
    this.children = [];
  }
  set className(value) { this._className = value; }
  get className() { return this._className || ""; }
  get classList() {
    const el = this;
    const tokens = () => el.className.split(/\s+/).filter(Boolean);
    return {
      contains: (name) => tokens().includes(name),
      add: (name) => { if (!tokens().includes(name)) el.className = (el.className + " " + name).trim(); },
      remove: (name) => { el.className = tokens().filter((t) => t !== name).join(" "); },
      toggle: (name, force) => {
        const on = force === undefined ? !tokens().includes(name) : Boolean(force);
        if (on) this.classList.add(name); else this.classList.remove(name);
        return on;
      },
    };
  }
  appendChild(child) {
    child.parentNode = this;
    this.children.push(child);
    return child;
  }
  setAttribute(name, value) {
    this.attributes.set(name, String(value));
    if (name.startsWith("data-")) {
      const key = name.slice(5).replace(/-([a-z])/g, (_, c) => c.toUpperCase());
      this.dataset[key] = String(value);
    }
  }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  toggleAttribute(name, force) {
    if (force) this.attributes.set(name, ""); else this.attributes.delete(name);
  }
  addEventListener(type, listener) {
    if (!this.listeners.has(type)) this.listeners.set(type, []);
    this.listeners.get(type).push(listener);
  }
  dispatch(type, event = {}) {
    for (const listener of this.listeners.get(type) || []) listener(event);
  }
  querySelectorAll(selector) {
    const out = [];
    const visit = (node) => {
      for (const child of node.children) {
        if (matches(child, selector)) out.push(child);
        visit(child);
      }
    };
    visit(this);
    const list = Object.assign(Object.create(null), {
      length: out.length,
      item: (i) => out[i] ?? null,
      forEach: out.forEach.bind(out),
      [Symbol.iterator]: out[Symbol.iterator].bind(out),
    });
    out.forEach((el, i) => { list[i] = el; });
    return list;
  }
  querySelector(selector) { return this.querySelectorAll(selector)[0] || null; }
  contains(node) {
    let probe = node;
    while (probe) { if (probe === this) return true; probe = probe.parentNode; }
    return false;
  }
  closest(selector) {
    let probe = this;
    while (probe) { if (matches(probe, selector)) return probe; probe = probe.parentNode; }
    return null;
  }
  showModal() { this.open = true; }
  close() { this.open = false; }
  focus() { this.focused = true; if (this.ownerDocument) this.ownerDocument.activeElement = this; }
}

function matches(el, selector) {
  const attr = selector.match(/^\[([a-zA-Z-]+)(?:="([^"]*)")?\]$/);
  if (attr) {
    let value = el.getAttribute(attr[1]);
    if (value === null && attr[1].startsWith("data-")) {
      const key = attr[1].slice(5).replace(/-([a-z])/g, (_, c) => c.toUpperCase());
      value = el.dataset[key] ?? null;
    }
    return attr[2] === undefined ? value !== null : value === attr[2];
  }
  if (selector.startsWith(".")) return (el.className || "").split(/\s+/).includes(selector.slice(1));
  return el.tagName === selector.toUpperCase();
}

function buildDocument() {
  const document = {
    _byAttr: new Map(),
    activeElement: null,
    createElement(tag) { return new MiniElement(tag, document); },
    querySelector(selector) {
      const probe = document._root;
      return probe ? probe.querySelector(selector) : null;
    },
    hidden: false,
  };
  const panel = new MiniElement("div", document);
  panel.setAttribute("data-config-view-panel", "skills");
  const list = new MiniElement("div", document);
  list.setAttribute("data-skills-list", "");
  const message = new MiniElement("p", document);
  message.setAttribute("data-skills-message", "");
  const meta = new MiniElement("span", document);
  meta.setAttribute("data-skills-meta", "");
  const count = new MiniElement("b", document);
  count.setAttribute("data-skills-count", "");
  const installedCount = new MiniElement("strong", document);
  installedCount.setAttribute("data-skills-installed-count", "");
  const info = new MiniElement("button", document);
  info.setAttribute("data-skills-info", "");
  const refreshButton = new MiniElement("button", document);
  refreshButton.setAttribute("data-skill-action", "refresh");
  const dialog = new MiniElement("dialog", document);
  dialog.setAttribute("data-skill-dialog", "");
  const dialogTitle = new MiniElement("h2", document);
  dialogTitle.setAttribute("data-skill-dialog-title", "");
  const dialogSummary = new MiniElement("p", document);
  dialogSummary.setAttribute("data-skill-dialog-summary", "");
  const dialogDiff = new MiniElement("div", document);
  dialogDiff.setAttribute("data-skill-dialog-diff", "");
  const dialogError = new MiniElement("p", document);
  dialogError.setAttribute("data-skill-dialog-error", "");
  const dialogConfirm = new MiniElement("button", document);
  dialogConfirm.setAttribute("data-skill-dialog-confirm", "");
  const dialogClose = new MiniElement("button", document);
  dialogClose.setAttribute("data-skill-dialog-close", "");
  for (const el of [list, message, meta, installedCount, info, refreshButton]) panel.appendChild(el);
  for (const el of [dialogTitle, dialogSummary, dialogDiff, dialogError, dialogConfirm, dialogClose]) dialog.appendChild(el);
  const root = new MiniElement("body", document);
  root.appendChild(panel);
  root.appendChild(dialog);
  root.appendChild(count);
  document._root = root;
  return { document, panel, list, message, meta, count, installedCount, info, refreshButton, dialog, dialogTitle, dialogSummary, dialogDiff, dialogConfirm, dialogClose };
}

function skillsStatus(overrides = {}) {
  return {
    installRoot: "/home/test/.agents/skills",
    catalogState: "fresh",
    catalogMessage: null,
    skills: [
      {
        id: "ai-cove-imagine",
        name: "AI Cove Imagine",
        description: "通过 AI Cove 生成图片和视频。",
        installedVersion: null,
        latestVersion: "1.0.0",
        localState: "absent",
        updateState: "available",
        localRevision: "rev-absent",
        releaseRevision: "rel-1",
        localChanges: [],
        latestDiff: [],
        backupPath: null,
        error: null,
      },
      {
        id: "managed-skill",
        name: "Managed",
        description: "installed",
        installedVersion: "1.0.0",
        latestVersion: "1.0.0",
        localState: "managed",
        updateState: "current",
        localRevision: "rev-managed",
        releaseRevision: "rel-2",
        localChanges: [],
        latestDiff: [],
        backupPath: null,
        error: null,
      },
      {
        id: "unmanaged-skill",
        name: "Unmanaged <script>",
        description: "raw <b>html</b> must stay text",
        installedVersion: null,
        latestVersion: "2.0.0",
        localState: "unmanaged",
        updateState: "available",
        localRevision: "rev-unmanaged",
        releaseRevision: "rel-3",
        localChanges: [],
        latestDiff: [{ path: "SKILL.md", kind: "modified" }],
        backupPath: null,
        error: null,
      },
    ],
    ...overrides,
  };
}

async function loadModule({ invoke }) {
  const source = await readFile(new URL("../src/skills.js", import.meta.url), "utf8");
  const dom = buildDocument();
  const context = {
    window: {
      __TAURI__: invoke ? { core: { invoke } } : undefined,
      setTimeout: (fn) => setTimeout(fn, 0),
      TurboSkills: undefined,
    },
    document: dom.document,
    console,
  };
  context.window.document = dom.document;
  vm.runInNewContext(source, context);
  return { TurboSkills: context.window.TurboSkills, dom };
}

function click(button, list) {
  list.dispatch("click", {
    target: button,
    preventDefault() {},
  });
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

test("skill actions share the header with status badges instead of a bottom row", async () => {
  const base = skillsStatus();
  base.skills.push({ ...base.skills[1], id: "update-skill", latestVersion: "1.1.0", updateState: "available" });
  const { TurboSkills, dom } = await loadModule({ invoke: async () => base });
  TurboSkills.activate(true);
  await tick(); await tick();
  for (const row of dom.list.children) {
    const head = row.querySelector(".b-skills-row__head");
    const controls = head.querySelector(".b-skills-row__controls");
    assert.ok(controls);
    assert.equal(controls.children[0].className, "b-skills-row__badges");
    const actions = row.querySelector(".b-skills-row__actions");
    assert.equal(actions.parentNode, controls);
    assert.equal(row.children.some((child) => child === actions), false);
  }
  const updated = dom.list.children.find((row) => row.dataset.skillId === "update-skill");
  assert.equal(updated.querySelector('[data-skill-action="install"]').textContent, "更新");
  assert.equal(updated.querySelector('[data-skill-action="uninstall"]').textContent, "卸载");
});

test("backup paths stay in expandable details, separate from version metadata", async () => {
  const base = skillsStatus();
  base.skills[1].backupPath = "/home/test/" + "long-path/".repeat(20) + "backup";
  const { TurboSkills, dom } = await loadModule({ invoke: async () => base });
  TurboSkills.activate(true);
  await tick(); await tick();
  const row = dom.list.children.find((child) => child.dataset.skillId === "managed-skill");
  assert.doesNotMatch(row.querySelector(".b-skills-row__meta").textContent, /backup|long-path/);
  const backup = row.querySelector("details");
  assert.equal(backup.open, false);
  assert.equal(backup.querySelector("code").textContent, base.skills[1].backupPath);
  backup.open = true;
  await TurboSkills.refresh(true);
  const updated = dom.list.children.find((child) => child.dataset.skillId === "managed-skill");
  assert.equal(updated.querySelector("details").open, true);
});

test("initial loading and failed refresh expose readable recovery states", async () => {
  let reject;
  const gate = new Promise((_, fail) => { reject = fail; });
  const { TurboSkills, dom } = await loadModule({ invoke: () => gate });
  TurboSkills.activate(true);
  assert.equal(dom.refreshButton.textContent, "正在检查…");
  assert.equal(dom.refreshButton.disabled, true);
  assert.equal(dom.list.getAttribute("aria-busy"), "true");
  assert.match(dom.list.children[0].textContent, /正在读取/);
  reject(new Error("offline"));
  await tick(); await tick();
  assert.equal(dom.refreshButton.textContent, "检查更新");
  assert.equal(dom.refreshButton.disabled, false);
  assert.equal(dom.list.getAttribute("aria-busy"), "false");
  assert.match(dom.meta.textContent, /offline/);
  assert.match(dom.list.children[0].textContent, /重试/);
});

test("activate loads status through real invoke and renders rows", async () => {
  const calls = [];
  const { TurboSkills, dom } = await loadModule({
    invoke: async (name, args) => {
      calls.push([name, args]);
      assert.equal(name, "get_skills");
      return skillsStatus();
    },
  });
  assert.ok(TurboSkills);
  TurboSkills.activate(true);
  await tick(); await tick();
  assert.equal(calls.length, 1);
  assert.equal(calls[0][1].refresh, false);
  const rows = dom.list.children.filter((child) => child.className.includes("b-skills-row"));
  assert.equal(rows.length, 3);
  assert.equal(dom.count.textContent, "3");
  assert.equal(dom.installedCount.textContent, "2");
  const unmanagedRow = rows.find((row) => row.dataset.skillId === "unmanaged-skill");
  assert.ok(unmanagedRow);
  assert.equal(unmanagedRow.children[0].children[0].children[0].textContent, "Unmanaged <script>");
});

test("install without confirmation invokes expected revisions", async () => {
  const calls = [];
  const { TurboSkills, dom } = await loadModule({
    invoke: async (name, args) => {
      calls.push([name, args]);
      if (name === "get_skills") return skillsStatus();
      const status = skillsStatus();
      status.skills[0].localState = "managed";
      status.skills[0].updateState = "current";
      status.skills[0].installedVersion = "1.0.0";
      return { status, backupPath: null };
    },
  });
  TurboSkills.activate(true);
  await tick(); await tick();
  const row = dom.list.children.find((child) => child.dataset.skillId === "ai-cove-imagine");
  const install = row.querySelectorAll('[data-skill-action="install"]')[0];
  assert.ok(install);
  click(install, dom.list);
  await tick(); await tick();
  const installCall = calls.find(([name]) => name === "install_skill");
  assert.ok(installCall);
  assert.equal(installCall[1].id, "ai-cove-imagine");
  assert.equal(installCall[1].expectedReleaseRevision, "rel-1");
  assert.equal(installCall[1].expectedLocalRevision, "rev-absent");
  assert.equal(installCall[1].confirmReplace, false);
  const updated = dom.list.children.find((child) => child.dataset.skillId === "ai-cove-imagine");
  assert.equal(updated.querySelectorAll('[data-local-state]')[0].dataset.localState, "managed");
});

test("unmanaged install opens dialog then confirms with confirmReplace", async () => {
  const calls = [];
  const { TurboSkills, dom } = await loadModule({
    invoke: async (name, args) => {
      calls.push([name, args]);
      if (name === "get_skills") return skillsStatus();
      return { status: skillsStatus(), backupPath: "/tmp/backup" };
    },
  });
  TurboSkills.activate(true);
  await tick(); await tick();
  const row = dom.list.children.find((child) => child.dataset.skillId === "unmanaged-skill");
  const install = row.querySelectorAll('[data-skill-action="install"]')[0];
  click(install, dom.list);
  await tick();
  assert.equal(dom.dialog.open, true);
  assert.match(dom.dialogTitle.textContent, /接管/);
  assert.equal(calls.some(([name]) => name === "install_skill"), false);
  dom.dialogConfirm.dispatch("click", { preventDefault() {} });
  await tick(); await tick();
  const installCall = calls.find(([name]) => name === "install_skill");
  assert.ok(installCall);
  assert.equal(installCall[1].confirmReplace, true);
  assert.equal(installCall[1].expectedLocalRevision, "rev-unmanaged");
  assert.equal(dom.message.textContent, "安装完成；原目录已备份，可展开“最近备份”查看。");
  const updatedRow = Array.from(dom.list.children).find((child) => child.dataset.skillId === "unmanaged-skill");
  assert.equal(updatedRow.querySelector('[data-skill-backup-path]').textContent, "/tmp/backup");
  assert.equal(dom.dialog.open, false);
});

test("uninstall always requires confirmation", async () => {
  const calls = [];
  const { TurboSkills, dom } = await loadModule({
    invoke: async (name, args) => {
      calls.push([name, args]);
      if (name === "get_skills") return skillsStatus();
      return { status: skillsStatus(), backupPath: "/tmp/u" };
    },
  });
  TurboSkills.activate(true);
  await tick(); await tick();
  const row = dom.list.children.find((child) => child.dataset.skillId === "managed-skill");
  const uninstall = row.querySelectorAll('[data-skill-action="uninstall"]')[0];
  click(uninstall, dom.list);
  await tick();
  assert.equal(dom.dialog.open, true);
  assert.match(dom.dialogTitle.textContent, /卸载/);
  dom.dialogConfirm.dispatch("click", { preventDefault() {} });
  await tick(); await tick();
  const uninstallCall = calls.find(([name]) => name === "uninstall_skill");
  assert.ok(uninstallCall);
  assert.equal(uninstallCall[1].id, "managed-skill");
  assert.equal(uninstallCall[1].expectedLocalRevision, "rev-managed");
  assert.equal(uninstallCall[1].confirmed, true);
});

test("revision_changed error refreshes status and shows message", async () => {
  const calls = [];
  const { TurboSkills, dom } = await loadModule({
    invoke: async (name, args) => {
      calls.push([name, args]);
      if (name === "get_skills") return skillsStatus();
      throw { code: "revision_changed", message: "本地内容在上次检查后发生变化" };
    },
  });
  TurboSkills.activate(true);
  await tick(); await tick();
  const row = dom.list.children.find((child) => child.dataset.skillId === "ai-cove-imagine");
  click(row.querySelectorAll('[data-skill-action="install"]')[0], dom.list);
  await tick(); await tick(); await tick();
  assert.match(dom.message.textContent, /发生变化/);
  assert.equal(calls.filter(([name]) => name === "get_skills").length >= 2, true);
});

test("local_conflict without confirm opens dialog instead of mutating", async () => {
  const calls = [];
  const { TurboSkills, dom } = await loadModule({
    invoke: async (name, args) => {
      calls.push([name, args]);
      if (name === "get_skills") return skillsStatus();
      if (args.confirmReplace !== true) throw { code: "local_conflict", message: "本地存在修改" };
      return { status: skillsStatus(), backupPath: "/tmp/x" };
    },
  });
  TurboSkills.activate(true);
  await tick(); await tick();
  const row = dom.list.children.find((child) => child.dataset.skillId === "ai-cove-imagine");
  click(row.querySelectorAll('[data-skill-action="install"]')[0], dom.list);
  await tick(); await tick();
  assert.equal(dom.dialog.open, true);
  dom.dialogConfirm.dispatch("click", { preventDefault() {} });
  await tick(); await tick();
  const installCalls = calls.filter(([name]) => name === "install_skill");
  assert.equal(installCalls.length, 2);
  assert.equal(installCalls[1][1].confirmReplace, true);
});

test("activation toggles loading only when visible", async () => {
  const calls = [];
  const { TurboSkills } = await loadModule({
    invoke: async (name) => { calls.push(name); return skillsStatus(); },
  });
  TurboSkills.activate(false);
  await tick();
  assert.equal(calls.length, 0);
  TurboSkills.activate(true);
  await tick(); await tick();
  assert.equal(calls.length, 1);
  TurboSkills.activate(false);
  TurboSkills.activate(true);
  await tick(); await tick();
  assert.equal(calls.length, 2);
});

test("pending mutation disables the row and ignores repeat clicks", async () => {
  const calls = [];
  let release;
  const gate = new Promise((resolve) => { release = resolve; });
  const { TurboSkills, dom } = await loadModule({
    invoke: async (name, args) => {
      calls.push(name);
      if (name === "get_skills") return skillsStatus();
      await gate;
      return { status: skillsStatus(), backupPath: null };
    },
  });
  TurboSkills.activate(true);
  await tick(); await tick();
  const row = dom.list.children.find((child) => child.dataset.skillId === "ai-cove-imagine");
  const install = row.querySelectorAll('[data-skill-action="install"]')[0];
  click(install, dom.list);
  await tick();
  const busyRow = dom.list.children.find((child) => child.dataset.skillId === "ai-cove-imagine");
  assert.equal(busyRow.getAttribute("aria-busy"), "true");
  const busyInstall = busyRow.querySelectorAll('[data-skill-action="install"]')[0];
  assert.equal(busyInstall.disabled, true);
  click(busyInstall, dom.list);
  release();
  await tick(); await tick();
  assert.equal(calls.filter((name) => name === "install_skill").length, 1);
});

test("stale get_skills response cannot overwrite a mutation result", async () => {
  let resolveSlow;
  const slow = new Promise((resolve) => { resolveSlow = resolve; });
  const calls = [];
  const { TurboSkills, dom } = await loadModule({
    invoke: async (name, args) => {
      calls.push(name);
      if (name === "get_skills") {
        if (args.refresh) { await slow; }
        return skillsStatus();
      }
      const next = skillsStatus();
      next.skills[0].localState = "managed";
      next.skills[0].updateState = "current";
      next.skills[0].installedVersion = "1.0.0";
      return { status: next, backupPath: "/tmp/b" };
    },
  });
  TurboSkills.activate(true);
  await tick(); await tick();
  TurboSkills.refresh(true);
  await tick();
  const row = dom.list.children.find((child) => child.dataset.skillId === "ai-cove-imagine");
  click(row.querySelectorAll('[data-skill-action="install"]')[0], dom.list);
  await tick(); await tick();
  const stale = skillsStatus();
  stale.skills[0].localState = "absent";
  resolveSlow(stale);
  await tick(); await tick();
  const rendered = dom.list.children.find((child) => child.dataset.skillId === "ai-cove-imagine");
  assert.equal(rendered.querySelectorAll('[data-local-state]')[0].dataset.localState, "managed");
});

test("unsafe and local_newer rows expose no install action", async () => {
  const base = skillsStatus();
  base.skills[0].localState = "unsafe";
  base.skills[1].localState = "managed";
  base.skills[1].updateState = "local_newer";
  base.skills[1].installedVersion = "9.9.9";
  base.skills[2].localState = "absent";
  base.catalogState = "stale";
  const { TurboSkills, dom } = await loadModule({
    invoke: async () => base,
  });
  TurboSkills.activate(true);
  await tick(); await tick();
  for (const row of dom.list.children) {
    const installButtons = row.querySelectorAll('[data-skill-action="install"]');
    assert.equal(installButtons.length, 0, `stale catalog must not offer install for ${row.dataset.skillId}`);
  }
  const unsafe = dom.list.children.find((child) => child.dataset.skillId === "ai-cove-imagine");
  assert.equal(unsafe.querySelectorAll('[data-skill-action="uninstall"]').length, 0);
});

test("unmanaged row reports unknown local version instead of absent", async () => {
  const { TurboSkills, dom } = await loadModule({
    invoke: async () => skillsStatus(),
  });
  TurboSkills.activate(true);
  await tick(); await tick();
  const row = dom.list.children.find((child) => child.dataset.skillId === "unmanaged-skill");
  const meta = row.children.find((child) => (child.className || "").includes("b-skills-row__meta"));
  assert.ok(meta.textContent.includes("本地版本未知"));
});

test("dialog cancel restores focus to a live row action", async () => {
  const { TurboSkills, dom } = await loadModule({
    invoke: async () => skillsStatus(),
  });
  TurboSkills.activate(true);
  await tick(); await tick();
  const row = dom.list.children.find((child) => child.dataset.skillId === "unmanaged-skill");
  const install = row.querySelectorAll('[data-skill-action="install"]')[0];
  click(install, dom.list);
  await tick();
  assert.equal(dom.dialog.open, true);
  dom.dialog.dispatch("cancel", {});
  dom.dialog.close();
  await tick();
  assert.equal(dom.dialog.open, false);
  const live = Array.from(dom.list.querySelectorAll('[data-skill-action="install"]'))
    .find((button) => button.dataset.skillId === "unmanaged-skill");
  assert.ok(live);
  assert.equal(dom.document.activeElement, live);
});

test("refresh during active mutation is deferred", async () => {
  let resolveMut;
  const gate = new Promise((resolve) => { resolveMut = resolve; });
  const calls = [];
  const { TurboSkills, dom } = await loadModule({
    invoke: async (name) => {
      calls.push(name);
      if (name === "get_skills") return skillsStatus();
      await gate;
      return { status: skillsStatus(), backupPath: null };
    },
  });
  TurboSkills.activate(true);
  await tick(); await tick();
  const row = dom.list.children.find((child) => child.dataset.skillId === "ai-cove-imagine");
  click(row.querySelectorAll('[data-skill-action="install"]')[0], dom.list);
  await tick();
  TurboSkills.refresh(true);
  await tick();
  assert.equal(calls.filter((name) => name === "get_skills").length, 1);
  resolveMut();
  await tick(); await tick();
  TurboSkills.refresh(true);
  await tick(); await tick();
  assert.equal(calls.filter((name) => name === "get_skills").length, 2);
});
