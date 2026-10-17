'use strict';

const assert = require('node:assert/strict');
const test = require('node:test');
const { ActionCoordinator, buildConfirmCloseAction, buildDraftSyncActions, createRequestIdFactory, panelIsVisible, reconcileInputDrafts, sanitizeRevokedSnapshot, shouldSendOnEnter, utf8ByteLength } = require('./app.js');

function makeSnapshot(overrides) {
  return Object.assign({ mode: 'offline', panels: [{ kind: 'chat', visibility: 'visible', confirmation: false }], chat: { draft: '', status: 'ready' } }, overrides || {});
}

test('Enter sends, Shift+Enter and IME composition do not', () => {
  assert.equal(shouldSendOnEnter({ key: 'Enter', shiftKey: false, isComposing: false, keyCode: 13 }), true);
  assert.equal(shouldSendOnEnter({ key: 'Enter', shiftKey: true, isComposing: false, keyCode: 13 }), false);
  assert.equal(shouldSendOnEnter({ key: 'Enter', shiftKey: false, isComposing: true, keyCode: 13 }), false);
  assert.equal(shouldSendOnEnter({ key: 'Enter', shiftKey: false, isComposing: false, keyCode: 229 }), false);
  assert.equal(shouldSendOnEnter({ key: 'a', shiftKey: false, isComposing: false }), false);
});

test('draft sync is omitted only when the snapshot already matches', () => {
  assert.deepEqual(buildDraftSyncActions('你好', ''), [{ action: 'set_draft', text: '你好' }]);
  assert.deepEqual(buildDraftSyncActions('same', 'same'), []);
  assert.equal(utf8ByteLength('a中🙂'), 8);
});

test('draft sync and send use distinct IDs in strict serial order', async () => {
  const events = [];
  let nextId = 0;
  const runner = new ActionCoordinator({
    idFactory: () => 'test-' + (++nextId),
    postAction: async (request) => {
      events.push('start:' + request.action + ':' + request.id);
      await new Promise((resolve) => setImmediate(resolve));
      events.push('end:' + request.action);
      return { kind: 'success', snapshot: makeSnapshot() };
    }
  });
  const actions = buildDraftSyncActions('hello', '').concat([{ action: 'send' }]);
  assert.deepEqual(await runner.run(actions), { kind: 'success' });
  assert.deepEqual(events.map((event) => event.split(':').slice(0, 2).join(':')), ['start:set_draft', 'end:set_draft', 'start:send', 'end:send']);
  assert.match(events[0], /^start:set_draft:test-1$/);
  assert.match(events[2], /^start:send:test-2$/);
});

test('request IDs are unique and match the host allowlist', () => {
  let sequence = 0;
  const makeId = createRequestIdFactory({ randomUUID: () => '00000000-0000-4000-8000-' + String(++sequence).padStart(12, '0') });
  const first = makeId();
  const second = makeId();
  assert.match(first, /^[A-Za-z0-9_-]{1,64}$/);
  assert.notEqual(first, second);
});

test('concurrent action transactions are rejected while one is in flight', async () => {
  let finishAction;
  let calls = 0;
  const runner = new ActionCoordinator({
    postAction: () => {
      calls += 1;
      return new Promise((resolve) => { finishAction = resolve; });
    },
    idFactory: () => 'single-request'
  });
  const first = runner.run([{ action: 'open', panel: 'memory' }]);
  assert.equal(runner.busy, true);
  assert.deepEqual(await runner.run([{ action: 'open', panel: 'memory' }]), { kind: 'busy' });
  assert.equal(calls, 1);
  finishAction({ kind: 'success', snapshot: makeSnapshot() });
  assert.deepEqual(await first, { kind: 'success' });
});

test('close confirmation is sent only when the server snapshot requests it', () => {
  const waiting = makeSnapshot();
  assert.equal(buildConfirmCloseAction(waiting, 'chat', true), null);
  const confirmed = makeSnapshot({ panels: [{ kind: 'chat', visibility: 'visible', confirmation: true }] });
  assert.deepEqual(buildConfirmCloseAction(confirmed, 'chat', true), { action: 'confirm_close', panel: 'chat', discard: true });
  assert.deepEqual(buildConfirmCloseAction(confirmed, 'chat', false), { action: 'confirm_close', panel: 'chat', discard: false });
});

test('panel visibility follows only the server visibility enum', () => {
  assert.equal(panelIsVisible({ visibility: 'visible' }), true);
  assert.equal(panelIsVisible({ visibility: 'hidden' }), false);
  assert.equal(panelIsVisible({ visibility: 'closed' }), false);
  assert.equal(panelIsVisible(null), false);
});

test('business errors stop the remaining plan and are not automatically retried', async () => {
  let calls = 0;
  let safeMessage = '';
  const runner = new ActionCoordinator({
    postAction: async () => {
      calls += 1;
      return { kind: 'business-error', message: '安全提示', snapshot: makeSnapshot() };
    },
    onOutcome: (result) => { if (result.kind === 'business-error') safeMessage = result.message; }
  });
  assert.deepEqual(await runner.run([{ action: 'set_draft', text: 'x' }, { action: 'send' }]), { kind: 'business-error' });
  assert.equal(calls, 1);
  assert.equal(safeMessage, '安全提示');
});

test('unknown action results block repeat posts until a successful state read', async () => {
  let calls = 0;
  const runner = new ActionCoordinator({
    postAction: async () => {
      calls += 1;
      if (calls === 1) throw new Error('network');
      return { kind: 'success', snapshot: makeSnapshot() };
    },
    idFactory: () => 'unique-' + calls
  });
  assert.deepEqual(await runner.run([{ action: 'send' }]), { kind: 'unknown' });
  assert.deepEqual(await runner.run([{ action: 'send' }]), { kind: 'blocked-unknown' });
  assert.equal(calls, 1);
  runner.reconcileAfterStateRead();
  assert.deepEqual(await runner.run([{ action: 'refresh_memories' }]), { kind: 'success' });
  assert.equal(calls, 2);
});

test('revocation overrides forced and business-error draft restoration while ordinary hidden snapshots preserve local drafts', () => {
  const revoked = Object.assign(makeSnapshot({ chat: { draft: 'server secret', status: 'ready', messages: [] } }), { revoked: true });
  assert.deepEqual(reconcileInputDrafts(revoked, 'local chat secret', 'local memory secret', 'older draft', null, true), { chatDraft: '', memoryDraft: '' });
  assert.deepEqual(reconcileInputDrafts(revoked, 'local chat secret', 'local memory secret', 'older draft', { action: 'set_draft' }, false, 'business-error'), { chatDraft: '', memoryDraft: '' });
  const hidden = makeSnapshot({ panels: [{ kind: 'chat', visibility: 'hidden', confirmation: false }], chat: { draft: 'server draft', status: 'ready', messages: [] } });
  assert.deepEqual(reconcileInputDrafts(hidden, 'unsynced chat draft', 'unsubmitted memory text', 'older draft', { action: 'hide' }, false), { chatDraft: 'unsynced chat draft', memoryDraft: 'unsubmitted memory text' });
});

test('revoked snapshots discard protected snapshot caches and close every panel', () => {
  const revoked = Object.assign(makeSnapshot({ panels: [{ kind: 'chat', visibility: 'visible', confirmation: true }] }), {
    revoked: true,
    presets: [{ id: 'secret-preset' }],
    active_preset: 'secret-preset',
    memories: [{ text: 'private memory' }],
    diagnostics: [{ event: 'private event' }],
    notice: 'private notice'
  });
  revoked.chat.draft = 'private draft';
  revoked.chat.messages = [{ role: 'user', text: 'private message' }];
  const sanitized = sanitizeRevokedSnapshot(revoked);
  assert.equal(sanitized.chat.draft, '');
  assert.deepEqual(sanitized.chat.messages, []);
  assert.deepEqual(sanitized.presets, []);
  assert.deepEqual(sanitized.memories, []);
  assert.deepEqual(sanitized.diagnostics, []);
  assert.equal(sanitized.notice, '');
  assert.ok(sanitized.panels.every((panel) => panel.visibility === 'closed' && panel.confirmation === false));
});

test('revoked action snapshots stop pending workflow actions before business-error handling', async () => {
  let calls = 0;
  const snapshots = [];
  const outcomes = [];
  const revoked = Object.assign(makeSnapshot(), { revoked: true });
  const runner = new ActionCoordinator({
    postAction: async () => {
      calls += 1;
      return { kind: 'business-error', message: 'revoked', snapshot: revoked };
    },
    onSnapshot: (snapshot) => snapshots.push(snapshot),
    onOutcome: (outcome) => outcomes.push(outcome)
  });
  assert.deepEqual(await runner.run([{ action: 'set_draft', text: 'private draft' }, { action: 'send' }]), { kind: 'revoked' });
  assert.equal(calls, 1);
  assert.deepEqual(snapshots, [revoked]);
  assert.deepEqual(outcomes, []);
});
