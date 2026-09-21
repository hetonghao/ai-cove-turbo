(function () {
  'use strict';

  const LOCAL_STATE_LABELS = {
    absent: '未安装',
    managed: '已安装',
    unmanaged: '未登记目录',
    modified: '本地已修改',
    unsafe: '路径不安全',
    error: '状态异常',
  };
  const UPDATE_STATE_LABELS = {
    available: '可安装',
    current: '已是最新',
    local_newer: '本地较新',
    unknown: '未知',
    unavailable: '不可用',
  };
  const CHANGE_KIND_LABELS = {
    added: '新增',
    modified: '修改',
    deleted: '删除',
  };

  let els = null;
  let status = null;
  let visible = false;
  let loaded = false;
  let loading = false;
  let previewData = null;
  const pendingBySkill = new Map();
  let pendingConfirm = null;
  let lastTriggerKey = null;
  let requestStamp = 0;
  let mutationStamp = 0;

  function collectElements() {
    const panel = document.querySelector('[data-config-view-panel="skills"]');
    if (!panel) return null;
    return {
      panel,
      list: panel.querySelector('[data-skills-list]'),
      message: panel.querySelector('[data-skills-message]'),
      meta: panel.querySelector('[data-skills-meta]'),
      count: document.querySelector('[data-skills-count]'),
      installedCount: panel.querySelector('[data-skills-installed-count]'),
      installRoot: panel.querySelector('[data-skills-install-root]'),
      info: panel.querySelector('[data-skills-info]'),
      refreshButton: panel.querySelector('[data-skill-action="refresh"]'),
      dialog: document.querySelector('[data-skill-dialog]'),
      dialogTitle: document.querySelector('[data-skill-dialog-title]'),
      dialogSummary: document.querySelector('[data-skill-dialog-summary]'),
      dialogDiff: document.querySelector('[data-skill-dialog-diff]'),
      dialogError: document.querySelector('[data-skill-dialog-error]'),
      dialogConfirm: document.querySelector('[data-skill-dialog-confirm]'),
    };
  }

  function invokeCommand(name, args) {
    const tauri = window.__TAURI__;
    if (tauri && tauri.core && typeof tauri.core.invoke === 'function') {
      return tauri.core.invoke(name, args || {});
    }
    return previewInvoke(name, args || {});
  }

  function previewInvoke(name, args) {
    if (!previewData) {
      previewData = {
        installRoot: '~/Library/Caches/preview/.agents/skills',
        catalogState: 'fresh',
        catalogMessage: null,
        skills: [
          {
            id: 'ai-cove-imagine',
            name: 'AI Cove Imagine',
            description: '通过 AI Cove 生成图片和视频，支持参考图、模型选择与本地优先级。',
            installedVersion: '1.0.0',
            latestVersion: '1.0.0',
            localState: 'managed',
            updateState: 'current',
            localRevision: 'preview-rev-1',
            releaseRevision: 'preview-rel-1',
            localChanges: [],
            latestDiff: [],
            backupPath: null,
            error: null,
          },
          {
            id: 'demo-unmanaged',
            name: 'Demo Unmanaged',
            description: 'Preview 预览数据：演示未登记目录接管流程。',
            installedVersion: null,
            latestVersion: '2.1.0',
            localState: 'unmanaged',
            updateState: 'available',
            localRevision: 'preview-rev-2',
            releaseRevision: 'preview-rel-2',
            localChanges: [{ path: 'SKILL.md', kind: 'modified' }],
            latestDiff: [
              { path: 'SKILL.md', kind: 'modified' },
              { path: 'scripts/tool.py', kind: 'added' },
            ],
            backupPath: null,
            error: null,
          },
        ],
      };
    }
    return new Promise((resolve) => {
      window.setTimeout(() => {
        if (name === 'get_skills') {
          resolve(previewData);
          return;
        }
        const skill = previewData.skills.find((entry) => entry.id === args.id);
        if (!skill) {
          resolve({ status: previewData, backupPath: null });
          return;
        }
        if (name === 'install_skill') {
          skill.localState = 'managed';
          skill.updateState = 'current';
          skill.installedVersion = skill.latestVersion;
          skill.localChanges = [];
          skill.latestDiff = [];
          skill.backupPath = '~/Library/Caches/preview/backups/' + skill.id;
          resolve({ status: previewData, backupPath: skill.backupPath });
          return;
        }
        if (name === 'uninstall_skill') {
          skill.localState = 'absent';
          skill.updateState = 'available';
          skill.installedVersion = null;
          skill.backupPath = '~/Library/Caches/preview/backups/' + skill.id;
          resolve({ status: previewData, backupPath: skill.backupPath });
        }
      }, 30);
    });
  }

  function isPreview() {
    const tauri = window.__TAURI__;
    return !(tauri && tauri.core && typeof tauri.core.invoke === 'function');
  }

  function localLabel(state) {
    return LOCAL_STATE_LABELS[state] || state;
  }

  function updateLabel(skill) {
    if (skill.updateState === 'current' && skill.localState === 'modified') {
      return '版本相同（本地已修改）';
    }
    return UPDATE_STATE_LABELS[skill.updateState] || skill.updateState;
  }

  function setActionMessage(text) {
    if (els && els.message) els.message.textContent = text || '';
  }

  function describeError(error) {
    if (!error) return '';
    if (typeof error === 'string') return error;
    if (error.message) return error.message;
    return String(error);
  }

  function renderChanges(changes, emptyText) {
    const details = document.createElement('details');
    details.className = 'b-skills-diff__details';
    const summary = document.createElement('summary');
    summary.textContent = emptyText + '（' + changes.length + '）';
    details.appendChild(summary);
    const list = document.createElement('ul');
    for (const change of changes) {
      const item = document.createElement('li');
      const kind = document.createElement('b');
      kind.textContent = CHANGE_KIND_LABELS[change.kind] || change.kind;
      kind.dataset.changeKind = change.kind;
      const path = document.createElement('code');
      path.textContent = change.path;
      item.appendChild(kind);
      item.appendChild(path);
      list.appendChild(item);
    }
    details.appendChild(list);
    return details;
  }

  function installActionLabel(skill) {
    if (skill.localState === 'absent') return '安装';
    if (skill.localState === 'unmanaged') return '接管安装';
    if (skill.localState === 'modified' || skill.localState === 'error') return '恢复官方版本';
    if (skill.installedVersion) return '更新';
    return '安装';
  }

  function installable(skill) {
    if (!status || status.catalogState !== 'fresh') return false;
    if (!skill.releaseRevision) return false;
    if (skill.localState === 'unsafe') return false;
    if (skill.updateState === 'local_newer') return false;
    if (skill.updateState === 'current') {
      return skill.localState === 'modified' || skill.localState === 'error';
    }
    return skill.updateState === 'available' || skill.localState === 'modified' || skill.localState === 'unmanaged' || skill.localState === 'error';
  }

  function captureUiState() {
    const open = [];
    if (els && els.list) {
      els.list.querySelectorAll('details').forEach((details) => {
        if (details.open) open.push(details.dataset.diffKey || '');
      });
    }
    const active = document.activeElement;
    let focusKey = null;
    if (active && active.dataset && active.dataset.skillAction && active.dataset.skillId) {
      focusKey = active.dataset.skillAction + ':' + active.dataset.skillId;
    }
    return { open, focusKey };
  }

  function restoreUiState(uiState) {
    if (!els || !els.list) return;
    els.list.querySelectorAll('details').forEach((details) => {
      if (uiState.open.includes(details.dataset.diffKey || '')) details.open = true;
    });
    if (uiState.focusKey) {
      const [action, id] = uiState.focusKey.split(':');
      const live = Array.from(els.list.querySelectorAll('[data-skill-action="' + action + '"]'))
        .find((button) => button.dataset.skillId === id);
      if (live && typeof live.focus === 'function') live.focus();
    }
  }

  function renderSkill(skill) {
    const row = document.createElement('article');
    row.className = 'b-skills-row';
    row.dataset.skillId = skill.id;

    const head = document.createElement('div');
    head.className = 'b-skills-row__head';
    const title = document.createElement('div');
    title.className = 'b-skills-row__title';
    const name = document.createElement('strong');
    name.textContent = skill.name;
    const idCode = document.createElement('code');
    idCode.textContent = skill.id;
    title.appendChild(name);
    title.appendChild(idCode);

    const badges = document.createElement('div');
    badges.className = 'b-skills-row__badges';
    const localBadge = document.createElement('span');
    localBadge.className = 'b-skills-badge';
    localBadge.dataset.localState = skill.localState;
    localBadge.textContent = localLabel(skill.localState);
    badges.appendChild(localBadge);
    const updateBadge = document.createElement('span');
    updateBadge.className = 'b-skills-badge b-skills-badge--update';
    updateBadge.dataset.updateState = skill.updateState;
    updateBadge.textContent = updateLabel(skill);
    badges.appendChild(updateBadge);
    const controls = document.createElement('div');
    controls.className = 'b-skills-row__controls';
    controls.appendChild(badges);
    head.appendChild(title);
    head.appendChild(controls);

    const meta = document.createElement('p');
    meta.className = 'b-skills-row__meta';
    const versions = [];
    versions.push(
      skill.installedVersion
        ? '已装 ' + skill.installedVersion
        : skill.localState === 'absent'
          ? '未安装'
          : '本地版本未知'
    );
    if (skill.latestVersion) {
      versions.push(
        (status && status.catalogState !== 'fresh' ? '上次获取版本 ' : '最新 ') + skill.latestVersion
      );
    }
    meta.textContent = versions.join(' · ');

    const desc = document.createElement('p');
    desc.className = 'b-skills-row__desc';
    desc.textContent = skill.description || '';

    row.appendChild(head);
    row.appendChild(meta);
    row.appendChild(desc);

    if (skill.backupPath) {
      const details = document.createElement('details');
      details.className = 'b-skills-diff__details b-skills-backup';
      details.dataset.diffKey = skill.id + ':backup';
      const summary = document.createElement('summary');
      summary.textContent = '最近备份';
      const path = document.createElement('code');
      path.dataset.skillBackupPath = '';
      path.textContent = skill.backupPath;
      details.appendChild(summary);
      details.appendChild(path);
      row.appendChild(details);
    }

    if (skill.error) {
      const error = document.createElement('p');
      error.className = 'b-skills-row__error';
      error.textContent = skill.error;
      row.appendChild(error);
    }
    if (skill.localChanges && skill.localChanges.length) {
      const details = renderChanges(skill.localChanges, '本地相对已装版本的变化');
      details.dataset.diffKey = skill.id + ':local';
      row.appendChild(details);
    }
    if (skill.latestDiff && skill.latestDiff.length) {
      const details = renderChanges(skill.latestDiff, '与最新版本的差异');
      details.dataset.diffKey = skill.id + ':latest';
      row.appendChild(details);
    }

    const actions = document.createElement('div');
    actions.className = 'b-skills-row__actions';
    const phase = document.createElement('span');
    phase.className = 'b-skills-row__phase';
    phase.dataset.skillPhase = skill.id;
    actions.appendChild(phase);

    const pending = pendingBySkill.get(skill.id);
    const busy = Boolean(pending);
    if (busy) {
      row.setAttribute('aria-busy', 'true');
      phase.textContent = pending === 'install' ? '正在安装…' : '正在卸载…';
    }

    if (installable(skill)) {
      const install = document.createElement('button');
      install.type = 'button';
      install.dataset.skillAction = 'install';
      install.dataset.skillId = skill.id;
      install.disabled = busy;
      install.textContent = installActionLabel(skill);
      actions.appendChild(install);
    }
    if (skill.localState !== 'absent' && skill.localState !== 'unsafe') {
      const uninstall = document.createElement('button');
      uninstall.type = 'button';
      uninstall.className = 'b-skills-danger';
      uninstall.dataset.skillAction = 'uninstall';
      uninstall.dataset.skillId = skill.id;
      uninstall.disabled = busy;
      uninstall.textContent = '卸载';
      actions.appendChild(uninstall);
    }
    if (busy || actions.children.length > 1) controls.appendChild(actions);
    return row;
  }

  function render() {
    if (!els || !status) return;
    const uiState = captureUiState();
    els.list.textContent = '';
    for (const skill of status.skills) {
      els.list.appendChild(renderSkill(skill));
    }
    if (!status.skills.length) {
      const empty = document.createElement('p');
      empty.className = 'b-skills-empty';
      empty.textContent =
        status.catalogState === 'unavailable'
          ? '官方目录暂不可用，请点击“检查更新”重试。'
          : '官方目录暂无可展示的技能。';
      els.list.appendChild(empty);
    }
    if (els.count) els.count.textContent = String(status.skills.length);
    if (els.installedCount) {
      els.installedCount.textContent = String(
        status.skills.filter((skill) => skill.localState !== 'absent').length
      );
    }
    if (els.installRoot) els.installRoot.textContent = status.installRoot || '';
    const metaParts = [];
    if (isPreview()) metaParts.push('Preview 预览数据，不会改动真实文件');
    if (status.catalogState === 'stale') metaParts.push('离线缓存目录，可能不是最新');
    if (status.catalogState === 'unavailable') metaParts.push('官方目录不可用，请检查网络后重试');
    if (status.catalogMessage) metaParts.push(status.catalogMessage);
    if (els.meta) els.meta.textContent = metaParts.join(' · ');
    if (els.info) {
      const tip = '技能安装目录：' + (status.installRoot || '未知');
      els.info.dataset.tooltip = tip;
      els.info.setAttribute('aria-label', tip);
    }
    restoreUiState(uiState);
  }

  function findSkill(id) {
    return status && status.skills.find((skill) => skill.id === id);
  }

  function focusLiveFallback() {
    if (!els || !els.list) return;
    if (lastTriggerKey) {
      const [action, id] = lastTriggerKey.split(':');
      const live = Array.from(els.list.querySelectorAll('[data-skill-action="' + action + '"]'))
        .find((button) => button.dataset.skillId === id);
      if (live) {
        live.focus();
        lastTriggerKey = null;
        return;
      }
    }
    const first = els.list.querySelector('[data-skill-action]');
    if (first) first.focus();
    else if (els.refreshButton) els.refreshButton.focus();
    lastTriggerKey = null;
  }

  function closeDialog() {
    if (els && els.dialog && els.dialog.open) els.dialog.close();
    pendingConfirm = null;
    focusLiveFallback();
  }

  function openDialog(options) {
    if (!els || !els.dialog) return false;
    els.dialogTitle.textContent = options.title;
    els.dialogSummary.textContent = options.summary || '';
    els.dialogDiff.textContent = '';
    if (options.changes && options.changes.length) {
      const list = document.createElement('ul');
      for (const change of options.changes) {
        const item = document.createElement('li');
        const kind = document.createElement('b');
        kind.textContent = CHANGE_KIND_LABELS[change.kind] || change.kind;
        kind.dataset.changeKind = change.kind;
        const path = document.createElement('code');
        path.textContent = change.path;
        item.appendChild(kind);
        item.appendChild(path);
        list.appendChild(item);
      }
      els.dialogDiff.appendChild(list);
    }
    els.dialogError.textContent = '';
    els.dialogConfirm.textContent = options.confirmText || '确认';
    els.dialogConfirm.classList.toggle('b-skills-danger', Boolean(options.danger));
    pendingConfirm = options;
    if (typeof els.dialog.showModal === 'function') {
      els.dialog.showModal();
      const cancel = els.dialog.querySelector('[data-skill-dialog-cancel]');
      if (cancel) cancel.focus();
    }
    return true;
  }

  function installDialogSummary(skill) {
    const parts = [
      '目标版本 ' + (skill.latestVersion || '未知'),
      '安装到 ' + ((status && status.installRoot) || '~/.agents/skills') + '/' + skill.id,
      '现有目录整体移入 .ai-cove-skills/backups 备份目录，设置与其他技能不受影响',
    ];
    return parts.join('；') + '。';
  }

  function confirmFor(action, skill) {
    if (action === 'uninstall') {
      return {
        title: '确认卸载技能？',
        summary:
          '「' +
          skill.name +
          '」将从 ' +
          ((status && status.installRoot) || '~/.agents/skills') +
          ' 移入 .ai-cove-skills/backups 备份目录；备份不会被自动删除，其他技能与设置不受影响。',
        confirmText: '确认卸载',
        changes: skill.localChanges,
        danger: true,
      };
    }
    if (skill.localState === 'unmanaged') {
      return {
        title: '接管同名目录？',
        summary: '本地已存在未登记的「' + skill.id + '」目录。' + installDialogSummary(skill),
        confirmText: '备份并安装',
        changes: skill.latestDiff,
      };
    }
    if (skill.localState === 'modified' || skill.localState === 'error') {
      return {
        title: '覆盖本地修改？',
        summary: '本地内容与官方记录不一致。' + installDialogSummary(skill),
        confirmText: '备份并覆盖',
        changes: skill.localChanges && skill.localChanges.length ? skill.localChanges : skill.latestDiff,
      };
    }
    return null;
  }

  async function mutate(action, skill, confirmed) {
    mutationStamp += 1;
    pendingBySkill.set(skill.id, action);
    render();
    let needsRefresh = false;
    try {
      const args =
        action === 'install'
          ? {
              id: skill.id,
              expectedReleaseRevision: skill.releaseRevision || '',
              expectedLocalRevision: skill.localRevision,
              confirmReplace: confirmed,
            }
          : {
              id: skill.id,
              expectedLocalRevision: skill.localRevision,
              confirmed: confirmed,
            };
      const result = await invokeCommand(action === 'install' ? 'install_skill' : 'uninstall_skill', args);
      mutationStamp += 1;
      if (result && result.status) {
        status = result.status;
      }
      const completed = action === 'install' ? '安装完成' : '卸载完成';
      if (result && result.backupPath) {
        const updated = findSkill(skill.id);
        if (updated) updated.backupPath = result.backupPath;
        setActionMessage(updated
          ? completed + '；原目录已备份，可展开“最近备份”查看。'
          : completed + '；已备份到 ' + result.backupPath);
      } else {
        setActionMessage(completed + '。');
      }
    } catch (error) {
      const message = describeError(error);
      const code = error && error.code;
      if (code === 'local_conflict' && !confirmed) {
        const dialogOptions = confirmFor(action, skill) || {
          title: '确认覆盖安装？',
          summary: (message || '本地目录状态已变化。') + installDialogSummary(skill),
          confirmText: '备份并安装',
          changes: skill.latestDiff,
        };
        if (openDialog(Object.assign({ action, skill }, dialogOptions))) {
          setActionMessage(message);
          return;
        }
      }
      setActionMessage(message);
      if (code === 'revision_changed') {
        needsRefresh = true;
      }
    } finally {
      pendingBySkill.delete(skill.id);
      render();
      if (!pendingBySkill.size) focusLiveFallback();
      if (needsRefresh) refresh(false);
    }
  }

  function refresh(manual) {
    if (!els || loading || pendingBySkill.size) return Promise.resolve();
    loading = true;
    if (els.refreshButton) {
      els.refreshButton.disabled = true;
      els.refreshButton.textContent = '正在检查…';
    }
    els.list.setAttribute('aria-busy', 'true');
    if (!status) {
      els.list.textContent = '';
      const placeholder = document.createElement('p');
      placeholder.className = 'b-skills-empty';
      placeholder.textContent = '正在读取官方技能目录…';
      els.list.appendChild(placeholder);
    }
    const stamp = requestStamp = requestStamp + 1;
    const mutationsAtStart = mutationStamp;
    return invokeCommand('get_skills', { refresh: Boolean(manual) })
      .then((next) => {
        if (stamp !== requestStamp || mutationsAtStart !== mutationStamp) return;
        status = next;
        loaded = true;
        render();
      })
      .catch((error) => {
        if (stamp !== requestStamp) return;
        if (els && els.meta) els.meta.textContent = '检查失败，请检查网络后重试。' + describeError(error);
        if (!status && els && els.list.children.length) {
          els.list.children[0].textContent = '暂时无法读取官方目录，请点击“检查更新”重试。';
        }
      })
      .finally(() => {
        loading = false;
        if (els) els.list.setAttribute('aria-busy', 'false');
        if (els && els.refreshButton) {
          els.refreshButton.disabled = false;
          els.refreshButton.textContent = '检查更新';
        }
      });
  }

  function bind() {
    if (!els || els.panel.dataset.skillsBound === '1') return;
    els.panel.dataset.skillsBound = '1';
    els.list.addEventListener('click', (event) => {
      const button = event.target.closest('[data-skill-action]');
      if (!button || !els.list.contains(button)) return;
      const action = button.dataset.skillAction;
      const skill = findSkill(button.dataset.skillId);
      if (!skill || pendingBySkill.has(skill.id)) return;
      if (action === 'install' || action === 'uninstall') {
        const dialogOptions = confirmFor(action, skill);
        const needsConfirm =
          action === 'uninstall' ||
          skill.localState === 'unmanaged' ||
          skill.localState === 'modified' ||
          skill.localState === 'error';
        if (needsConfirm && dialogOptions) {
          lastTriggerKey = action + ':' + skill.id;
          openDialog(Object.assign({ action, skill }, dialogOptions));
          return;
        }
        mutate(action, skill, false);
      }
    });
    if (els.refreshButton) {
      els.refreshButton.addEventListener('click', () => {
        setActionMessage('');
        refresh(true);
      });
    }
    if (els.dialog) {
      for (const closer of els.dialog.querySelectorAll('[data-skill-dialog-close]')) {
        closer.addEventListener('click', (event) => {
          event.preventDefault();
          closeDialog();
        });
      }
      els.dialog.addEventListener('cancel', () => {
        pendingConfirm = null;
        focusLiveFallback();
      });
      els.dialogConfirm.addEventListener('click', (event) => {
        event.preventDefault();
        const pending = pendingConfirm;
        pendingConfirm = null;
        if (els.dialog.open) els.dialog.close();
        if (pending) mutate(pending.action, pending.skill, true);
      });
    }
  }

  function activate(nextVisible) {
    if (!els) {
      els = collectElements();
      if (!els) return;
      bind();
    }
    const nowVisible = Boolean(nextVisible);
    const becameVisible = nowVisible && !visible;
    visible = nowVisible;
    if (nowVisible && (!loaded || becameVisible)) {
      refresh(false);
    }
  }

  window.TurboSkills = { activate, refresh };
})();
