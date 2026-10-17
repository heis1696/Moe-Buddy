(function (root) {
  'use strict';

  const MAX_TEXT_BYTES = 8000;
  const PANEL_LABELS = { chat: '聊天', presets: '模型预设', memory: '记忆', diagnostics: '运行日志' };

  function shouldSendOnEnter(event) {
    return event.key === 'Enter' && !event.shiftKey && !event.isComposing && event.keyCode !== 229;
  }

  function panelIsVisible(panel) {
    return Boolean(panel && panel.visibility === 'visible');
  }

  function utf8ByteLength(value) {
    let length = 0;
    for (const character of String(value)) {
      const point = character.codePointAt(0);
      length += point <= 0x7f ? 1 : point <= 0x7ff ? 2 : point <= 0xffff ? 3 : 4;
    }
    return length;
  }

  function buildDraftSyncActions(text, serverDraft) {
    return text === (serverDraft || '') ? [] : [{ action: 'set_draft', text }];
  }

  function sanitizeRevokedSnapshot(snapshot) {
    return Object.assign({}, snapshot, {
      panels: snapshot.panels.map((panel) => panel && typeof panel === 'object' ? Object.assign({}, panel, { visibility: 'closed', confirmation: false }) : panel),
      chat: Object.assign({}, snapshot.chat, { draft: '', status: 'ready', failure: '', messages: [] }),
      presets: [],
      active_preset: null,
      memories: [],
      diagnostics: [],
      notice: ''
    });
  }

  function reconcileInputDrafts(snapshot, localChatDraft, localMemoryDraft, previousDraft, action, forceDraft, resultKind) {
    if (snapshot.revoked === true) return { chatDraft: '', memoryDraft: '' };
    const serverDraft = snapshot.chat.draft || '';
    const hadLocalDraft = localChatDraft !== previousDraft;
    const rejectedDraft = resultKind === 'business-error' && action && action.action === 'set_draft';
    const restoreServerDraft = forceDraft || (!rejectedDraft && (!hadLocalDraft || (action && (action.action === 'set_draft' || action.action === 'send'))));
    return { chatDraft: restoreServerDraft ? serverDraft : localChatDraft, memoryDraft: localMemoryDraft };
  }

  function buildConfirmCloseAction(snapshot, panel, discard) {
    if (typeof discard !== 'boolean' || !snapshot || !Array.isArray(snapshot.panels)) return null;
    const target = snapshot.panels.find((item) => item && item.kind === panel);
    return target && target.confirmation === true ? { action: 'confirm_close', panel, discard } : null;
  }

  function createRequestIdFactory(cryptoApi) {
    const cryptoSource = cryptoApi || root.crypto;
    let sequence = 0;
    return function nextRequestId() {
      sequence += 1;
      if (cryptoSource && typeof cryptoSource.randomUUID === 'function') return 'act-' + cryptoSource.randomUUID();
      if (cryptoSource && typeof cryptoSource.getRandomValues === 'function') {
        const bytes = new Uint8Array(16);
        cryptoSource.getRandomValues(bytes);
        const token = Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
        return 'act-' + token + '-' + sequence.toString(36);
      }
      return 'act-' + Date.now().toString(36) + '-' + sequence.toString(36) + '-' + Math.random().toString(36).slice(2, 12);
    };
  }

  class ActionCoordinator {
    constructor(options) {
      this.postAction = options.postAction;
      this.onSnapshot = options.onSnapshot || function () {};
      this.onBusy = options.onBusy || function () {};
      this.onOutcome = options.onOutcome || function () {};
      this.idFactory = options.idFactory || createRequestIdFactory();
      this._busy = false;
      this._uncertain = false;
    }

    get busy() { return this._busy; }
    get uncertain() { return this._uncertain; }

    async run(actions) {
      if (this._busy) return { kind: 'busy' };
      if (this._uncertain) return { kind: 'blocked-unknown' };
      if (!actions.length) return { kind: 'empty' };
      this._busy = true;
      this.onBusy(true);
      try {
        for (const action of actions) {
          let result;
          try {
            result = await this.postAction(Object.assign({ id: this.idFactory() }, action));
          } catch (_error) {
            result = { kind: 'unknown' };
          }
          if (result && result.snapshot) this.onSnapshot(result.snapshot, action, result.kind);
          if (result && result.snapshot && result.snapshot.revoked === true) {
            this._uncertain = false;
            return { kind: 'revoked' };
          }
          if (result && result.kind === 'business-error' && result.snapshot) {
            this.onOutcome({ kind: 'business-error', message: result.message || '操作未完成，请查看服务端状态。' });
            return { kind: 'business-error' };
          }
          if (!result || result.kind !== 'success' || !result.snapshot) {
            this._uncertain = true;
            this.onOutcome({ kind: 'unknown' });
            return { kind: 'unknown' };
          }
        }
        this.onOutcome({ kind: 'success' });
        return { kind: 'success' };
      } finally {
        this._busy = false;
        this.onBusy(false);
      }
    }

    reconcileAfterStateRead() {
      this._uncertain = false;
    }
  }

  const testApi = { ActionCoordinator, buildConfirmCloseAction, buildDraftSyncActions, createRequestIdFactory, panelIsVisible, reconcileInputDrafts, sanitizeRevokedSnapshot, shouldSendOnEnter, utf8ByteLength };
  if (typeof module !== 'undefined' && module.exports) module.exports = testApi;
  if (typeof document !== 'undefined') startApplication(root.document);

  function startApplication(doc) {
    const byId = (id) => doc.getElementById(id);
    const chatInput = byId('chat-input');
    const memoryInput = byId('memory-input');
    const state = { csrf: null, snapshot: null, bootstrapping: true, refreshing: false, preparing: false, feedback: '', focusPendingConfirmation: false };

    function panelState(kind) {
      return state.snapshot && Array.isArray(state.snapshot.panels) ? state.snapshot.panels.find((panel) => panel && panel.kind === kind) : null;
    }

    function hasConfirmation() {
      return Boolean(state.snapshot && state.snapshot.panels && state.snapshot.panels.some((panel) => panel && panel.confirmation === true));
    }

    function baseBlocked() {
      return state.bootstrapping || state.refreshing || state.preparing || coordinator.busy || coordinator.uncertain || !state.csrf || !state.snapshot || state.snapshot.revoked === true;
    }

    function mutationsBlocked() {
      return baseBlocked() || hasConfirmation();
    }

    function showFeedback(message) {
      state.feedback = message || '';
      const region = byId('feedback');
      region.textContent = state.feedback;
      region.hidden = !state.feedback;
    }

    function validSnapshot(value) {
      return Boolean(value && typeof value === 'object' && Array.isArray(value.panels) && value.chat && typeof value.chat === 'object' && typeof value.mode === 'string');
    }

    async function readJson(response) {
      try { return await response.json(); } catch (_error) { return null; }
    }

    async function postAction(envelope) {
      let response;
      try {
        response = await root.fetch('/api/action', { method: 'POST', credentials: 'same-origin', cache: 'no-store', headers: { 'Content-Type': 'application/json', 'X-CSRF-Token': state.csrf }, body: JSON.stringify(envelope) });
      } catch (_error) {
        return { kind: 'unknown' };
      }
      const body = await readJson(response);
      if (!body || !validSnapshot(body.snapshot)) return { kind: 'unknown' };
      if (body.error && typeof body.error.message === 'string') return { kind: 'business-error', message: body.error.message.slice(0, 300), snapshot: body.snapshot };
      if (response.ok) return { kind: 'success', snapshot: body.snapshot };
      return { kind: 'unknown', snapshot: body.snapshot };
    }

    const coordinator = new ActionCoordinator({
      postAction,
      onSnapshot: (next, action, resultKind) => renderSnapshot(next, action, false, resultKind),
      onBusy: () => renderControls(),
      onOutcome: (result) => {
        if (result.kind === 'business-error') showFeedback(result.message);
        else if (result.kind === 'unknown') showFeedback('网络连接中断，操作结果未知；请勿自动重发，只刷新状态。');
        else if (result.kind === 'success') showFeedback('服务器已确认操作。');
        renderControls();
      }
    });

    function renderControls() {
      const blocked = mutationsBlocked();
      doc.querySelectorAll('[data-panel-nav]').forEach((button) => { button.disabled = blocked; });
      doc.querySelectorAll('[data-panel-hide], [data-panel-close], [data-preset-id]').forEach((button) => { button.disabled = blocked; });
      doc.querySelectorAll('[data-refresh-state]').forEach((button) => { button.disabled = state.bootstrapping || state.refreshing || state.preparing || coordinator.busy; });
      byId('chat-input').disabled = blocked || !panelIsVisible(panelState('chat'));
      byId('memory-input').disabled = blocked || !panelIsVisible(panelState('memory'));
      byId('send-button').disabled = blocked || !canSendDraft();
      byId('remember-button').disabled = blocked || !canRemember();
      byId('refresh-memories').disabled = blocked || !panelIsVisible(panelState('memory'));
      byId('refresh-diagnostics').disabled = blocked || !panelIsVisible(panelState('diagnostics'));
      byId('cancel-close').disabled = baseBlocked() || !hasConfirmation();
      byId('discard-close').disabled = baseBlocked() || !hasConfirmation();
      byId('unknown-banner').hidden = !coordinator.uncertain;
      byId('refresh-state').textContent = state.refreshing ? '正在刷新…' : coordinator.uncertain ? '只刷新状态' : '刷新状态';
      if (state.focusPendingConfirmation && !baseBlocked() && hasConfirmation()) {
        state.focusPendingConfirmation = false;
        byId('discard-close').focus();
      }
    }

    function canSendDraft() {
      const text = chatInput.value;
      const status = state.snapshot && state.snapshot.chat ? state.snapshot.chat.status : '';
      return Boolean(text.trim()) && utf8ByteLength(text) <= MAX_TEXT_BYTES && status !== 'sending' && status !== 'unknown';
    }

    function canRemember() {
      return Boolean(memoryInput.value.trim()) && utf8ByteLength(memoryInput.value) <= MAX_TEXT_BYTES;
    }

    function updateByteCount(input, output) {
      const size = utf8ByteLength(input.value);
      output.textContent = size + ' / ' + MAX_TEXT_BYTES + ' 字节';
      output.classList.toggle('byte-count--over', size > MAX_TEXT_BYTES);
      renderControls();
    }

    function node(tag, className, text) {
      const element = doc.createElement(tag);
      if (className) element.className = className;
      if (text !== undefined) element.textContent = text;
      return element;
    }

    function renderMessages(messages) {
      const list = byId('message-list');
      const empty = byId('empty-chat-state');
      list.querySelectorAll('.message').forEach((message) => message.remove());
      if (!messages.length) {
        empty.hidden = false;
        return;
      }
      empty.hidden = true;
      messages.forEach((message) => {
        if (!message || (message.role !== 'user' && message.role !== 'assistant')) return;
        const item = node('article', 'message message--' + message.role);
        item.append(node('span', 'message-role', message.role === 'user' ? '你' : '伙伴'));
        item.append(node('p', 'message-text', typeof message.text === 'string' ? message.text : ''));
        list.append(item);
      });
    }

    function renderPresets(presets, activePreset) {
      const list = byId('preset-list');
      list.replaceChildren();
      if (!presets.length) {
        list.append(node('p', 'empty-copy', '服务端当前未提供离线预设。'));
        return;
      }
      presets.forEach((preset) => {
        if (!preset || typeof preset.id !== 'string') return;
        const button = node('button', 'preset-option');
        button.type = 'button';
        button.dataset.presetId = preset.id;
        button.disabled = mutationsBlocked();
        button.setAttribute('aria-pressed', String(preset.id === activePreset));
        const head = node('span', 'preset-option__head');
        head.append(node('strong', '', preset.label || preset.id));
        head.append(node('span', 'preset-state', preset.id === activePreset ? '当前使用' : '离线'));
        button.append(head);
        button.append(node('span', 'preset-meta', [preset.provider, preset.model].filter(Boolean).join(' · ')));
        button.append(node('span', 'preset-endpoint', preset.endpoint || ''));
        list.append(button);
      });
    }

    function renderRecordList(listId, records, type) {
      const list = byId(listId);
      list.replaceChildren();
      if (!records.length) {
        list.append(node('li', 'empty-copy', type === 'memory' ? '当前没有可显示的记忆。' : '当前没有可显示的日志事件。'));
        return;
      }
      records.forEach((record) => {
        if (!record) return;
        const item = node('li', 'record-item');
        if (type === 'memory') {
          item.append(node('span', 'record-copy', record.text || ''));
        } else {
          item.append(node('span', 'record-sequence', typeof record.sequence === 'number' ? '#' + record.sequence : '事件'));
          item.append(node('strong', 'record-event', record.event || ''));
          item.append(node('span', 'record-meta', [record.run, record.status].filter(Boolean).join(' · ')));
        }
        list.append(item);
      });
    }

    function renderMetrics(panels) {
      const list = byId('panel-metrics');
      list.replaceChildren();
      panels.forEach((panel) => {
        if (!panel || !PANEL_LABELS[panel.kind]) return;
        const title = node('dt', '', PANEL_LABELS[panel.kind]);
        const counts = node('dd', '', '挂载 ' + panel.mounts + ' · 渲染 ' + panel.renders + ' · 卸载 ' + panel.unmounts);
        list.append(title, counts);
      });
    }

    function renderConfirmation() {
      const pending = state.snapshot.panels.find((panel) => panel && panel.confirmation === true);
      const layer = byId('close-confirmation');
      const wasHidden = layer.hidden;
      layer.hidden = !pending;
      if (!pending) {
        state.focusPendingConfirmation = false;
        return;
      }
      if (wasHidden) state.focusPendingConfirmation = true;
      layer.dataset.panel = pending.kind;
      byId('confirmation-title').textContent = '确认关闭' + (PANEL_LABELS[pending.kind] || '面板') + '？';
      byId('confirmation-copy').textContent = '服务端要求确认关闭。继续将丢弃待处理内容；取消则保留面板。';
    }

    function renderSnapshot(next, action, forceDraft, resultKind) {
      if (!validSnapshot(next)) return;
      const snapshot = next.revoked === true ? sanitizeRevokedSnapshot(next) : next;
      const previousDraft = state.snapshot && state.snapshot.chat ? state.snapshot.chat.draft || '' : '';
      const inputs = reconcileInputDrafts(snapshot, chatInput.value, memoryInput.value, previousDraft, action, forceDraft, resultKind);
      state.snapshot = snapshot;
      chatInput.value = inputs.chatDraft;
      memoryInput.value = inputs.memoryDraft;
      if (snapshot.revoked === true) {
        state.feedback = '';
        byId('feedback').textContent = '';
        byId('feedback').hidden = true;
        state.focusPendingConfirmation = false;
      }
      byId('mode-status').textContent = snapshot.mode === 'offline' ? '离线验收' : '模式：' + snapshot.mode;
      byId('transmission-status').textContent = snapshot.external_transmission_allowed === false ? '未授权外传' : snapshot.external_transmission_allowed === true ? '已授权外传' : '外传状态未知';
      byId('companion-heading').textContent = snapshot.companion || '聊天';
      byId('snapshot-revision').textContent = '快照 · ' + snapshot.revision;
      const notice = typeof snapshot.notice === 'string' ? snapshot.notice.slice(0, 300) : '';
      byId('snapshot-notice').textContent = notice;
      byId('snapshot-notice').hidden = !notice;
      ['chat', 'presets', 'memory', 'diagnostics'].forEach((kind) => {
        const panel = panelState(kind);
        const section = byId('panel-' + kind);
        section.hidden = !panelIsVisible(panel);
        const nav = doc.querySelector('[data-panel-nav=' + kind + ']');
        const stateLabel = doc.querySelector('[data-panel-state=' + kind + ']');
        if (nav) nav.setAttribute('aria-pressed', String(panelIsVisible(panel)));
        if (stateLabel) stateLabel.textContent = !panel ? '不可用' : panel.visibility === 'visible' ? '已显示' : panel.visibility === 'hidden' ? '已隐藏' : '已关闭';
      });
      const anyVisible = snapshot.panels.some(panelIsVisible);
      byId('all-panels-hidden').hidden = anyVisible;
      byId('panel-grid').hidden = !anyVisible;
      byId('panel-grid').dataset.chatVisible = String(panelIsVisible(panelState('chat')));
      const chat = snapshot.chat;
      const chatStatus = chat.status === 'sending' ? '正在处理请求…' : chat.status === 'failed' ? '请求失败 · ' + (chat.failure || '请检查服务端状态。') : chat.status === 'unknown' ? '结果未知 · 请先刷新状态' : '就绪 · 离线验收';
      byId('chat-status-label').textContent = chatStatus;
      byId('chat-context').dataset.status = chat.status;
      renderMessages(Array.isArray(chat.messages) ? chat.messages : []);
      renderPresets(Array.isArray(snapshot.presets) ? snapshot.presets : [], snapshot.active_preset);
      renderRecordList('memory-list', Array.isArray(snapshot.memories) ? snapshot.memories : [], 'memory');
      renderRecordList('diagnostics-list', Array.isArray(snapshot.diagnostics) ? snapshot.diagnostics : [], 'diagnostics');
      renderMetrics(snapshot.panels);
      renderConfirmation();
      byId('connection-banner').hidden = true;
      updateByteCount(chatInput, byId('composer-count'));
      updateByteCount(memoryInput, byId('memory-count'));
      renderControls();
    }

    async function getEndpoint(path) {
      let response;
      try {
        response = await root.fetch(path, { method: 'GET', credentials: 'same-origin', cache: 'no-store', headers: { Accept: 'application/json' } });
      } catch (_error) {
        return null;
      }
      const body = await readJson(response);
      return response.ok && body ? body : null;
    }

    function showConnectionIssue(message) {
      byId('connection-copy').textContent = message;
      byId('connection-banner').hidden = false;
    }

    async function bootstrap() {
      const body = await getEndpoint('/api/bootstrap');
      if (!body || typeof body.csrf !== 'string' || !validSnapshot(body.snapshot)) return false;
      state.csrf = body.csrf;
      renderSnapshot(body.snapshot, null, true);
      return true;
    }

    async function refreshState() {
      if (state.bootstrapping || state.refreshing || state.preparing || coordinator.busy) return;
      state.refreshing = true;
      renderControls();
      const wasUncertain = coordinator.uncertain;
      try {
        const body = await getEndpoint(state.csrf ? '/api/state' : '/api/bootstrap');
        const next = body && (body.snapshot || body);
        if (!validSnapshot(next)) {
          showConnectionIssue('状态读取失败。没有重发任何操作；可再次手动刷新状态。');
          showFeedback('只读状态刷新失败。');
          return;
        }
        if (!state.csrf && typeof body.csrf === 'string') state.csrf = body.csrf;
        coordinator.reconcileAfterStateRead();
        renderSnapshot(next, null, false);
        showFeedback(wasUncertain ? '状态已刷新，没有重发操作。请核对消息后再继续。' : '已读取最新服务端状态；没有提交操作。');
      } finally {
        state.refreshing = false;
        renderControls();
      }
    }

    function runWorkflow(actions, syncChatDraft) {
      if (baseBlocked() || state.preparing) return Promise.resolve({ kind: 'blocked' });
      if (hasConfirmation() && !actions.some((action) => action.action === 'confirm_close')) return Promise.resolve({ kind: 'confirmation-pending' });
      state.preparing = true;
      renderControls();
      const plan = [];
      if (syncChatDraft) {
        const draft = chatInput.value;
        if (utf8ByteLength(draft) > MAX_TEXT_BYTES) {
          state.preparing = false;
          renderControls();
          showFeedback('草稿超过 8000 字节；缩短后再继续。');
          return Promise.resolve({ kind: 'validation-error' });
        }
        plan.push.apply(plan, buildDraftSyncActions(draft, state.snapshot.chat.draft));
      }
      plan.push.apply(plan, actions);
      return coordinator.run(plan).finally(() => {
        state.preparing = false;
        renderControls();
      });
    }

    function changePanel(kind, action) {
      const body = { action, panel: kind };
      return runWorkflow([body], kind === 'chat' && (action === 'hide' || action === 'close'));
    }

    function sendCurrentDraft() {
      if (!canSendDraft()) {
        showFeedback(utf8ByteLength(chatInput.value) > MAX_TEXT_BYTES ? '消息超过 8000 字节，请缩短后发送。' : '请输入消息后再发送。');
        return Promise.resolve({ kind: 'validation-error' });
      }
      return runWorkflow([{ action: 'send' }], true);
    }

    function confirmClose(discard) {
      const pending = state.snapshot && state.snapshot.panels.find((panel) => panel && panel.confirmation === true);
      const action = pending && buildConfirmCloseAction(state.snapshot, pending.kind, discard);
      if (!action) return Promise.resolve({ kind: 'no-confirmation' });
      return runWorkflow([action], false);
    }

    byId('chat-form').addEventListener('submit', (event) => {
      event.preventDefault();
      void sendCurrentDraft();
    });
    chatInput.addEventListener('keydown', (event) => {
      if (!shouldSendOnEnter(event)) return;
      event.preventDefault();
      void sendCurrentDraft();
    });
    chatInput.addEventListener('input', () => updateByteCount(chatInput, byId('composer-count')));
    memoryInput.addEventListener('input', () => updateByteCount(memoryInput, byId('memory-count')));

    byId('remember-form').addEventListener('submit', (event) => {
      event.preventDefault();
      if (!canRemember()) {
        showFeedback(utf8ByteLength(memoryInput.value) > MAX_TEXT_BYTES ? '记忆内容超过 8000 字节，请缩短后提交。' : '请先输入要明确记住的内容。');
        return;
      }
      const text = memoryInput.value;
      void runWorkflow([{ action: 'remember', text }], false).then((result) => {
        if (result.kind === 'success') {
          memoryInput.value = '';
          updateByteCount(memoryInput, byId('memory-count'));
        }
      });
    });

    doc.addEventListener('click', (event) => {
      const target = event.target && event.target.closest ? event.target : null;
      if (!target) return;
      const refreshButton = target.closest('[data-refresh-state]');
      if (refreshButton) {
        void refreshState();
        return;
      }
      const navButton = target.closest('[data-panel-nav]');
      if (navButton) {
        const kind = navButton.dataset.panelNav;
        const action = panelIsVisible(panelState(kind)) ? 'hide' : 'open';
        void changePanel(kind, action);
        return;
      }
      const hideButton = target.closest('[data-panel-hide]');
      if (hideButton) {
        void changePanel(hideButton.dataset.panelHide, 'hide');
        return;
      }
      const closeButton = target.closest('[data-panel-close]');
      if (closeButton) {
        void changePanel(closeButton.dataset.panelClose, 'close');
        return;
      }
      const presetButton = target.closest('[data-preset-id]');
      if (presetButton) {
        if (state.snapshot.active_preset !== presetButton.dataset.presetId) void runWorkflow([{ action: 'select_preset', preset: presetButton.dataset.presetId }], false);
        return;
      }
      if (target.closest('#refresh-memories')) {
        void runWorkflow([{ action: 'refresh_memories' }], false);
        return;
      }
      if (target.closest('#refresh-diagnostics')) {
        void runWorkflow([{ action: 'refresh_diagnostics' }], false);
      }
    });

    byId('cancel-close').addEventListener('click', () => { void confirmClose(false); });
    byId('discard-close').addEventListener('click', () => { void confirmClose(true); });

    renderControls();
    void bootstrap().then((ok) => {
      state.bootstrapping = false;
      if (!ok) showConnectionIssue('无法读取本机会话。确认本机宿主正在运行后，只刷新状态。');
      renderControls();
    }).catch((_error) => {
      state.bootstrapping = false;
      showConnectionIssue('无法读取本机会话。确认本机宿主正在运行后，只刷新状态。');
      renderControls();
    });
  }
})(typeof globalThis !== 'undefined' ? globalThis : this);
