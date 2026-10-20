const test = require('node:test');
const assert = require('node:assert/strict');

const {
  RevisionOrderedSnapshots,
  parseSnapshot,
  parseSnapshotResponse,
} = require('../.test-build/snapshot.js');
const { IPC_CONTRACT } = require('../.test-build/contract.js');
const {
  buddyCommitMatchesIntent,
  parseBuddyCommit,
} = require('../.test-build/buddy.js');
const {
  credentialStatusText,
  displayActivePreset,
  displayDiagnostics,
  displayDraft,
  displayExternalTransmissionAllowed,
  displayMemories,
  displayMessages,
  displayModelSettings,
  displayNotice,
  displayPersistenceEnabled,
  displayPresets,
  displaySessionCatalog,
  displaySessionIsolationAllowed,
  displaySessions,
  failureText,
  persistenceHint,
  statusText,
  transmissionStatusText,
} = require('../.test-build/projection.js');

function makeSnapshot(revision, overrides = {}) {
  const base = {
    revision,
    presentationRevision: overrides.presentationRevision ?? revision,
    mode: 'offline',
    revoked: false,
    closeConfirmation: false,
    closeIntent: null,
    managementDraft: { version: 0, dirty: false, kind: 'none' },
    managementExit: null,
    managementDiscardedVersion: null,
    chatDraft: { revision, discard: null, sessions: [] },
    activePreset: 'default',
    externalTransmissionAllowed: false,
    presets: [],
    memories: [],
    diagnostics: [],
    notice: null,
    modelSettings: null,
    persistenceEnabled: true,
    activeSessionId: 'session-a',
    sessionsRevision: 3,
    sessions: [
      { id: 'session-a', label: '会话 A', archived: false, status: 'ready' },
    ],
    sessionIsolationAllowed: false,
    activeBuddyId: 'buddy-a',
    buddiesRevision: 2,
    activeBuddyPersona: '伙伴 A 的合成人设',
    buddyCommit: null,
    buddies: [
      { id: 'buddy-a', label: '伙伴 A', profileRevision: 1, personaRevision: 1, modelRevision: 1 },
    ],
    chat: {
      draft: '',
      messages: [],
      status: 'ready',
      failure: null,
    },
  };
  const chat = { ...base.chat, ...(overrides.chat ?? {}) };
  const snapshot = { ...base, ...overrides, chat };
  // 撤权快照必须同时清空伙伴注册表；严格解析不接受撤权后仍有档案。
  if (snapshot.revoked === true) {
    snapshot.activeBuddyId = null;
    snapshot.buddies = [];
    snapshot.activeBuddyPersona = null;
  }
  return snapshot;
}

function success(store, revision, chat = {}) {
  return store.applyResponse({ snapshot: makeSnapshot(revision, { chat }), error: null });
}

function failure(store, revision, chat = {}) {
  return store.applyResponse({
    snapshot: makeSnapshot(revision, { chat }),
    error: { code: 'host_timeout', message: 'host did not complete the command', disposition: 'unknown' },
  });
}

test('shared fixture and same-business-revision host intent update', () => {
  const store = new RevisionOrderedSnapshots();
  assert.equal(store.apply(makeSnapshot(7, { presentationRevision: 7 })), true);
  assert.equal(store.apply(makeSnapshot(7, {
    presentationRevision: 8,
    closeConfirmation: true,
    closeIntent: 'quit_application',
  })), true);
  assert.equal(store.current.closeIntent, 'quit_application');
  assert.deepEqual(Object.keys(makeSnapshot(1)).sort(), [...IPC_CONTRACT.snapshotFields].sort());
  assert.ok(IPC_CONTRACT.snapshotFields.includes('sessionIsolationAllowed'));
  assert.deepEqual(IPC_CONTRACT.dispositions, ['not_executed', 'settled', 'unknown']);
  assert.equal(IPC_CONTRACT.frameCount * IPC_CONTRACT.frameDurationMs, IPC_CONTRACT.clipDurationMs);
});

test('请求与事件乱序时旧 revision 不能覆盖新状态', () => {
  const store = new RevisionOrderedSnapshots();
  assert.equal(success(store, 2, { draft: 'old' }).kind, 'success');
  assert.equal(success(store, 5, { draft: 'new' }).kind, 'success');
  assert.equal(success(store, 3, { draft: 'stale-event' }).kind, 'success');
  assert.equal(store.current.revision, 5);
  assert.equal(store.current.chat.draft, 'new');
});

test('监听先注册、此后才拉初始快照时，较新的既有事件保持权威', () => {
  const store = new RevisionOrderedSnapshots();
  store.applyResponse({
    snapshot: makeSnapshot(9, {
      chat: { status: 'failed', failure: 'send_failed' },
    }),
    error: null,
  });
  const initial = store.applyResponse({
    snapshot: makeSnapshot(8, { chat: { status: 'ready' } }),
    error: null,
  });
  assert.equal(initial.applied, false);
  assert.equal(store.current.revision, 9);
  assert.equal(store.current.chat.status, 'failed');
});

test('严格 envelope：裸 snapshot、缺失字段与空对空全部拒绝', () => {
  const snapshot = makeSnapshot(1);
  assert.deepEqual(parseSnapshotResponse({ snapshot, error: null }), {
    snapshot,
    error: null,
  });

  assert.equal(parseSnapshotResponse(snapshot), null);
  assert.equal(parseSnapshotResponse({ snapshot }), null);
  assert.equal(parseSnapshotResponse({ error: null }), null);
  assert.equal(parseSnapshotResponse({ snapshot: null }), null);
  assert.equal(parseSnapshotResponse({ snapshot: null, error: null }), null);
  assert.equal(parseSnapshotResponse({ snapshot, error: 'boom' }), null);
  assert.equal(parseSnapshotResponse({ snapshot, error: { code: 1, message: 'x' } }), null);
  assert.equal(parseSnapshotResponse({ snapshot, error: { code: 'x', message: 'x', disposition: 'maybe' } }), null);
  assert.equal(parseSnapshotResponse({ snapshot, error: { code: 'x', message: 'x', disposition: 'unknown', extra: true } }), null);
  assert.equal(parseSnapshotResponse({ snapshot, error: { code: 'x' } }), null);
  assert.deepEqual(
    parseSnapshotResponse({ snapshot: [{ revision: 1 }], error: null }),
    null,
  );
});

test('来源拒绝只返回安全错误，不附聊天快照', () => {
  const rejected = parseSnapshotResponse({
    snapshot: null,
    error: { code: 'command_source', message: 'chat commands are main-window only', disposition: 'not_executed' },
  });
  assert.equal(rejected.snapshot, null);
  assert.deepEqual(rejected.error, {
    code: 'command_source',
    message: 'chat commands are main-window only',
    disposition: 'not_executed',
  });
});

test('快照字段按冻结结构校验，chat.failure 只接受字符串或 null', () => {
  const snapshot = makeSnapshot(1);
  assert.deepEqual(parseSnapshot(snapshot), snapshot);

  assert.equal(parseSnapshot({ ...snapshot, revision: -1 }), null);
  assert.equal(parseSnapshot({ ...snapshot, revision: 1.5 }), null);
  assert.equal(parseSnapshot({ ...snapshot, closeConfirmation: undefined }), null);
  assert.equal(parseSnapshot({ ...snapshot, extra: true }), null);
  assert.equal(parseSnapshot({ ...snapshot, closeConfirmation: true }), null);
  assert.equal(
    parseSnapshot({ ...snapshot, chat: { ...snapshot.chat, messages: [{ role: 'user' }] } }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...snapshot, chat: { ...snapshot.chat, failure: 'send_failed' } }).chat.failure,
    'send_failed',
  );
  assert.equal(
    parseSnapshot({
      ...snapshot,
      chat: { ...snapshot.chat, failure: { code: 'send_failed', message: '失败' } },
    }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...snapshot, chat: { ...snapshot.chat, failure: undefined } }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...snapshot, chat: { ...snapshot.chat, failure: ['x'] } }),
    null,
  );
});

test('buddyCommit 是必填无正文回执：严格字段、动作、ID、修订与管理版本', () => {
  const receipt = {
    kind: 'configure_model',
    fromBuddyId: 'buddy-a',
    fromRevision: 2,
    toBuddyId: 'buddy-a',
    toRevision: 3,
    managementVersion: 7,
  };
  const committed = makeSnapshot(3, { buddiesRevision: 3, buddyCommit: receipt });
  assert.deepEqual(parseSnapshot(committed).buddyCommit, receipt);
  assert.deepEqual(parseBuddyCommit(receipt, false), receipt);
  assert.equal(parseBuddyCommit(null, false), null);
  assert.equal(parseSnapshot(makeSnapshot(1)).buddyCommit, null);

  const { fromBuddyId, ...withoutFrom } = receipt;
  assert.equal(parseSnapshot({ ...committed, buddyCommit: withoutFrom }), null);
  assert.equal(parseSnapshot({ ...committed, buddyCommit: { ...receipt, extra: true } }), null);
  assert.equal(parseSnapshot({ ...committed, buddyCommit: { ...receipt, kind: 'delete_buddy' } }), null);
  assert.equal(parseSnapshot({ ...committed, buddyCommit: { ...receipt, fromBuddyId: '' } }), null);
  assert.equal(parseSnapshot({ ...committed, buddyCommit: { ...receipt, toBuddyId: '../buddy' } }), null);
  assert.equal(parseSnapshot({ ...committed, buddyCommit: { ...receipt, fromRevision: 0 } }), null);
  assert.equal(parseSnapshot({ ...committed, buddyCommit: { ...receipt, toRevision: 4 } }), null);
  assert.equal(parseSnapshot({ ...committed, buddyCommit: { ...receipt, managementVersion: -1 } }), null);
  assert.equal(
    parseSnapshot({ ...committed, buddyCommit: { ...receipt, managementVersion: 1.5 } }),
    null,
  );
  assert.equal(
    parseSnapshot({
      ...committed,
      buddyCommit: { ...receipt, managementVersion: Number.MAX_SAFE_INTEGER + 1 },
    }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...committed, revoked: true, buddyCommit: receipt }),
    null,
  );
  assert.equal(parseBuddyCommit(receipt, true), undefined);
  assert.deepEqual(IPC_CONTRACT.buddyCommitFields, [
    'kind',
    'fromBuddyId',
    'fromRevision',
    'toBuddyId',
    'toRevision',
    'managementVersion',
  ]);
  assert.deepEqual(IPC_CONTRACT.buddyCommitKinds, [
    'create_buddy',
    'switch_buddy',
    'rename_buddy',
    'configure_model',
    'select_preset',
  ]);
});

test('buddyCommit 只按动作、发起伙伴/修订与管理版本精确匹配本次意图', () => {
  const receipt = {
    kind: 'configure_model',
    fromBuddyId: 'buddy-a',
    fromRevision: 2,
    toBuddyId: 'buddy-a',
    toRevision: 3,
    managementVersion: 7,
  };
  const intent = {
    kind: 'configure_model',
    fromBuddyId: 'buddy-a',
    fromRevision: 2,
    managementVersion: 7,
  };
  assert.equal(buddyCommitMatchesIntent(intent, receipt), true);
  assert.equal(buddyCommitMatchesIntent(intent, null), false);
  assert.equal(buddyCommitMatchesIntent({ ...intent, kind: 'select_preset' }, receipt), false);
  assert.equal(buddyCommitMatchesIntent({ ...intent, fromBuddyId: 'buddy-b' }, receipt), false);
  assert.equal(buddyCommitMatchesIntent({ ...intent, fromRevision: 1 }, receipt), false);
  assert.equal(buddyCommitMatchesIntent({ ...intent, managementVersion: 6 }, receipt), false);
  assert.equal(
    buddyCommitMatchesIntent(intent, { ...receipt, toRevision: 9 }),
    false,
  );
});

test('管理编辑快照只接受安全版本、脏布尔值和当前退出意图', () => {
  const pending = makeSnapshot(4, {
    managementDraft: { version: 8, dirty: true, kind: 'general' },
    managementExit: { version: 8, intent: 'quit_application' },
  });
  assert.deepEqual(parseSnapshot(pending), pending);
  assert.equal(parseSnapshot({ ...pending, managementDraft: { version: 8, dirty: 'yes' } }), null);
  assert.equal(parseSnapshot({ ...pending, managementDraft: { version: 8, dirty: true, text: 'secret' } }), null);
  assert.equal(parseSnapshot({ ...pending, managementExit: { version: 9, intent: 'quit_application' } }), null);
  assert.equal(parseSnapshot({ ...pending, managementExit: { version: 8, intent: 'exit' } }), null);
  assert.equal(
    parseSnapshot({ ...pending, managementDiscardedVersion: Number.MAX_SAFE_INTEGER + 1 }),
    null,
  );
});

test('管理草稿 kind 与 activeBuddyPersona 是必填且互相一致的冻结字段', () => {
  const valid = makeSnapshot(1);
  assert.notEqual(parseSnapshot(valid), null);
  assert.equal(valid.managementDraft.kind, 'none');
  assert.equal(valid.activeBuddyPersona, '伙伴 A 的合成人设');

  // dirty=false 只能是 none；dirty=true 必须是三种编辑之一。
  assert.equal(parseSnapshot({ ...valid, managementDraft: { version: 1, dirty: true } }), null);
  assert.equal(
    parseSnapshot({ ...valid, managementDraft: { version: 1, dirty: true, kind: 'none' } }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...valid, managementDraft: { version: 1, dirty: false, kind: 'general' } }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...valid, managementDraft: { version: 1, dirty: true, kind: 'other' } }),
    null,
  );
  assert.notEqual(
    parseSnapshot({ ...valid, managementDraft: { version: 1, dirty: true, kind: 'buddy_create' } }),
    null,
  );
  assert.notEqual(
    parseSnapshot({ ...valid, managementDraft: { version: 1, dirty: true, kind: 'buddy_rename' } }),
    null,
  );

  // 当前档案人设必填；无活动伙伴/撤权时必须为 null。
  const withoutPersona = { ...valid };
  delete withoutPersona.activeBuddyPersona;
  assert.equal(parseSnapshot(withoutPersona), null);
  assert.equal(parseSnapshot({ ...valid, activeBuddyPersona: null }), null);
  assert.equal(parseSnapshot({ ...valid, activeBuddyPersona: '' }), null);
  assert.equal(parseSnapshot({ ...valid, activeBuddyPersona: '坏\u0000人设' }), null);
  assert.notEqual(parseSnapshot({ ...valid, activeBuddyPersona: '多行\n人设\t' }), null);
  assert.notEqual(
    parseSnapshot({
      ...valid,
      activeBuddyId: null,
      buddies: [],
      activeBuddyPersona: null,
    }),
    null,
  );
  assert.equal(
    parseSnapshot({
      ...valid,
      activeBuddyId: null,
      buddies: [],
      activeBuddyPersona: '无伙伴的人设',
    }),
    null,
  );
  const revoked = makeSnapshot(2, { revoked: true, activeSessionId: null, sessions: [] });
  assert.notEqual(parseSnapshot(revoked), null);
  assert.equal(parseSnapshot({ ...revoked, activeBuddyPersona: '残留人设' }), null);
});

test('旧 revision 的错误不能覆盖较新的成功状态', () => {
  const store = new RevisionOrderedSnapshots();
  assert.equal(success(store, 10, { status: 'ready', draft: 'newest' }).kind, 'success');

  const staleError = failure(store, 8, { status: 'unknown', failure: 'send_unknown' });
  assert.equal(staleError.kind, 'failure');
  assert.equal(staleError.stale, true);
  assert.equal(staleError.snapshot, null);
  assert.equal(store.current.revision, 10);
  assert.equal(store.current.chat.status, 'ready');
  assert.equal(store.current.chat.draft, 'newest');

  const currentError = failure(store, 11, { status: 'failed', failure: 'send_failed' });
  assert.equal(currentError.stale, false);
  assert.equal(currentError.snapshot.revision, 11);
  assert.equal(store.current.chat.status, 'failed');

  assert.equal(success(store, 12, { status: 'ready' }).kind, 'success');
  assert.equal(store.current.chat.status, 'ready');
});

test('来源拒绝错误不带 revision，也不推进快照状态', () => {
  const store = new RevisionOrderedSnapshots();
  success(store, 4, { draft: 'keep' });
  const rejected = store.applyResponse({
    snapshot: null,
    error: { code: 'command_source', message: 'chat commands are main-window only', disposition: 'not_executed' },
  });
  assert.equal(rejected.kind, 'failure');
  assert.equal(rejected.stale, false);
  assert.equal(rejected.snapshot, null);
  assert.equal(store.current.revision, 4);
  assert.equal(store.current.chat.draft, 'keep');
});

test('同 revision 的错误不会把成功快照回退', () => {
  const store = new RevisionOrderedSnapshots();
  success(store, 5, { draft: 'stable' });
  const error = failure(store, 5, { draft: 'stable' });
  assert.equal(error.kind, 'failure');
  assert.equal(error.stale, false);
  assert.equal(error.snapshot.revision, 5);
  assert.equal(store.current.revision, 5);
  assert.equal(store.current.chat.draft, 'stable');
});

test('撤权快照即使残留消息，界面投影也必须清空', () => {
  const revoked = makeSnapshot(6, {
    revoked: true,
    chat: {
      draft: '不应显示',
      messages: [{ role: 'user', text: '旧消息' }],
      status: 'ready',
    },
  });
  assert.deepEqual(displayMessages(revoked), []);
  assert.equal(displayDraft(revoked), '');
});

test('失败码投影给出明确副作用指引，未知状态不诱导重发', () => {
  assert.equal(failureText(null), null);
  assert.equal(failureText('  '), null);
  assert.equal(failureText('纯文本失败'), '纯文本失败');
  assert.match(failureText('send_unknown'), /请勿重复发送/);
  assert.match(failureText('send_unknown'), /结果未知/);
  assert.equal(statusText('unknown'), '结果未知');
  assert.equal(statusText('ready'), '就绪');
  assert.equal(statusText('custom_state'), 'custom_state');
});

test('新增快照字段严格解析；模型配置不得夹带 apiKey', () => {
  const full = makeSnapshot(2, {
    activePreset: 'p1',
    externalTransmissionAllowed: true,
    presets: [
      { id: 'p1', label: '本地预设', provider: 'offline', model: 'synthetic', endpoint: 'http://127.0.0.1:9' },
    ],
    memories: [{ id: '7', revision: 3, text: '喜欢 tea' }],
    diagnostics: [{ sequence: 0, run: 'run-1', event: 'stage_started', status: 'completed' }],
    notice: '旧存档损坏，已保留原文件',
    modelSettings: {
      endpoint: 'http://127.0.0.1:9',
      model: 'synthetic',
      persona: '苔',
      allowTransmission: true,
      hasApiKey: true,
    },
    persistenceEnabled: false,
  });
  assert.deepEqual(parseSnapshot(full), full);

  assert.equal(
    parseSnapshot({ ...full, modelSettings: { ...full.modelSettings, apiKey: 'SECRET' } }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...full, modelSettings: { ...full.modelSettings, hasApiKey: 'yes' } }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...full, modelSettings: { endpoint: 'x', model: 'm' } }),
    null,
  );
  assert.deepEqual(parseSnapshot({ ...full, modelSettings: null }).modelSettings, null);

  assert.equal(
    parseSnapshot({ ...full, presets: [{ ...full.presets[0], extra: true }] }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...full, memories: [{ ...full.memories[0], extra: true }] }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...full, diagnostics: [{ ...full.diagnostics[0], extra: true }] }),
    null,
  );
  assert.equal(parseSnapshot({ ...full, notice: { text: 'x' } }), null);
  assert.equal(parseSnapshot({ ...full, persistenceEnabled: 'yes' }), null);
  assert.equal(parseSnapshot({ ...full, activePreset: 1 }), null);
  assert.equal(parseSnapshot({ ...full, externalTransmissionAllowed: 1 }), null);
});

test('业务序号继续走安全整数门，不接受越界或负版本', () => {
  const unsafe = Number.MAX_SAFE_INTEGER + 1;
  assert.equal(parseSnapshot(makeSnapshot(unsafe)), null);
  assert.equal(parseSnapshot(makeSnapshot(1, { presentationRevision: unsafe })), null);
  assert.equal(
    parseSnapshot(makeSnapshot(1, { memories: [{ id: '1', revision: unsafe, text: 'x' }] })),
    null,
  );
  assert.equal(
    parseSnapshot(makeSnapshot(1, { memories: [{ id: '1', revision: -1, text: 'x' }] })),
    null,
  );
  assert.equal(
    parseSnapshot(makeSnapshot(1, { diagnostics: [{ sequence: -1, run: 'r', event: 'e', status: 's' }] })),
    null,
  );
  assert.equal(
    parseSnapshot(makeSnapshot(1, { diagnostics: [{ sequence: unsafe, run: 'r', event: 'e', status: 's' }] })),
    null,
  );
});

test('新增业务字段的共享夹具不做兼容放宽', () => {
  assert.deepEqual(IPC_CONTRACT.panels, ['presets', 'memory', 'diagnostics']);
  assert.deepEqual(IPC_CONTRACT.presetFields, ['id', 'label', 'provider', 'model', 'endpoint']);
  assert.deepEqual(IPC_CONTRACT.memoryFields, ['id', 'revision', 'text']);
  assert.deepEqual(IPC_CONTRACT.diagnosticFields, ['sequence', 'run', 'event', 'status']);
  assert.deepEqual(IPC_CONTRACT.modelSettingsFields, [
    'endpoint',
    'model',
    'persona',
    'allowTransmission',
    'hasApiKey',
  ]);
  assert.ok(!IPC_CONTRACT.modelSettingsFields.includes('apiKey'));
});

test('撤权后模型配置、预设、记忆与诊断投影全部清空', () => {
  const revoked = makeSnapshot(6, {
    revoked: true,
    activePreset: 'p1',
    externalTransmissionAllowed: true,
    presets: [{ id: 'p1', label: 'L', provider: 'offline', model: 'm', endpoint: 'e' }],
    memories: [{ id: '1', revision: 1, text: '私密' }],
    diagnostics: [{ sequence: 0, run: 'r', event: 'e', status: 's' }],
    notice: '宿主恢复提示',
    modelSettings: {
      endpoint: 'e',
      model: 'm',
      persona: 'p',
      allowTransmission: true,
      hasApiKey: true,
    },
    persistenceEnabled: true,
  });
  assert.equal(displayModelSettings(revoked), null);
  assert.deepEqual(displayPresets(revoked), []);
  assert.deepEqual(displayMemories(revoked), []);
  assert.deepEqual(displayDiagnostics(revoked), []);
  assert.equal(displayActivePreset(revoked), '');
  assert.equal(displayExternalTransmissionAllowed(revoked), false);
  assert.equal(displayPersistenceEnabled(revoked), false);
  assert.equal(persistenceHint(revoked), null);
  // 恢复提示是宿主的固定操作文案，不是凭据或业务正文，撤权后仍可展示。
  assert.equal(displayNotice(revoked), '宿主恢复提示');
});

test('本地恢复提示与凭据状态只显示非敏感投影', () => {
  assert.equal(displayNotice(makeSnapshot(1)), null);
  assert.equal(displayNotice(makeSnapshot(1, { notice: '   ' })), null);
  assert.equal(displayNotice(makeSnapshot(1, { notice: '  坏档已保留  ' })), '坏档已保留');
  assert.match(persistenceHint(makeSnapshot(1, { persistenceEnabled: false })), /不会写入磁盘/);
  assert.equal(persistenceHint(makeSnapshot(1, { persistenceEnabled: true })), null);
  assert.equal(credentialStatusText(null), '未配置模型');
  assert.match(
    credentialStatusText({
      endpoint: 'e',
      model: 'm',
      persona: 'p',
      allowTransmission: true,
      hasApiKey: true,
    }),
    /不会回显或落盘/,
  );
  assert.equal(transmissionStatusText(false), '未允许外传');
  assert.equal(transmissionStatusText(true), '允许向模型端点外传');
});

function sessionCatalog(overrides = {}) {
  return {
    activeSessionId: 'session-a',
    sessionsRevision: 3,
    sessions: [
      { id: 'session-a', label: '会话 A', archived: false, status: 'ready' },
      { id: 'session-b', label: '会话 B', archived: true, status: 'unknown' },
    ],
    ...overrides,
  };
}

test('A 会话目录三元组与 sessionIsolationAllowed 必填、严格且与撤权一致', () => {
  const withSessions = makeSnapshot(4, sessionCatalog());
  assert.deepEqual(parseSnapshot(withSessions), withSessions);
  assert.equal(parseSnapshot(withSessions).sessionIsolationAllowed, false);
  assert.equal(displaySessionIsolationAllowed(withSessions), false);
  assert.equal(
    displaySessionIsolationAllowed({ ...withSessions, sessionIsolationAllowed: true }),
    true,
  );
  assert.equal(
    displaySessionIsolationAllowed({ ...withSessions, revoked: true, sessionIsolationAllowed: true }),
    false,
  );

  // 缺任一必填字段的旧 D-only 快照整份拒绝；不再保留临时双版本兼容。
  for (const field of [...IPC_CONTRACT.sessionSnapshotFields, 'sessionIsolationAllowed']) {
    const legacy = makeSnapshot(1);
    delete legacy[field];
    assert.equal(parseSnapshot(legacy), null, `missing ${field}`);
  }

  for (const field of IPC_CONTRACT.sessionSnapshotFields) {
    assert.equal(parseSnapshot({ ...withSessions, [field]: undefined }), null, field);
  }
  assert.equal(parseSnapshot({ ...withSessions, activeSessionId: 'missing' }), null);
  assert.equal(parseSnapshot({ ...withSessions, sessionsRevision: -1 }), null);
  assert.equal(
    parseSnapshot({ ...withSessions, sessionsRevision: Number.MAX_SAFE_INTEGER + 1 }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...withSessions, sessions: [...withSessions.sessions, withSessions.sessions[0]] }),
    null,
  );
  assert.equal(
    parseSnapshot({
      ...withSessions,
      sessions: [{ ...withSessions.sessions[0], status: 'archived' }],
    }),
    null,
  );
  assert.equal(
    parseSnapshot({
      ...withSessions,
      sessions: [{ ...withSessions.sessions[0], archived: 'yes' }],
    }),
    null,
  );
  assert.equal(
    parseSnapshot({
      ...withSessions,
      sessions: [{ ...withSessions.sessions[0], text: '正文' }],
    }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...withSessions, sessions: [{ ...withSessions.sessions[0], id: 'bad id' }] }),
    null,
  );
  assert.equal(
    parseSnapshot({
      ...withSessions,
      sessions: [{ ...withSessions.sessions[0], id: 'x'.repeat(65) }],
    }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...withSessions, activeSessionId: null }),
    null,
    '有目录却没有当前选择必须拒绝',
  );
  const emptyCatalog = parseSnapshot({ ...withSessions, activeSessionId: null, sessions: [] });
  assert.equal(emptyCatalog.activeSessionId, null);
  assert.deepEqual(emptyCatalog.sessions, []);
  assert.equal(
    parseSnapshot({ ...withSessions, activeSessionId: null, sessions: [], sessionIsolationAllowed: true }),
    null,
    '没有当前会话时不能声称可另开隔离会话',
  );

  assert.equal(parseSnapshot({ ...withSessions, sessionIsolationAllowed: 'yes' }), null);
  assert.equal(parseSnapshot({ ...withSessions, sessionIsolationAllowed: 1 }), null);
  assert.equal(parseSnapshot({ ...withSessions, sessionIsolationAllowed: null }), null);
  assert.equal(
    parseSnapshot({ ...withSessions, revoked: true, sessionIsolationAllowed: true }),
    null,
  );
});

test('撤权后会话 ID 必须为 null、目录为空；目录最多 64 项', () => {
  const revoked = makeSnapshot(5, {
    revoked: true,
    ...sessionCatalog({ activeSessionId: null, sessions: [] }),
  });
  assert.deepEqual(parseSnapshot(revoked), revoked);
  assert.equal(
    parseSnapshot({ ...revoked, sessions: sessionCatalog().sessions }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...revoked, activeSessionId: 'session-a' }),
    null,
  );
  const full = makeSnapshot(6, sessionCatalog({
    sessions: Array.from({ length: 64 }, (_, index) => ({
      id: `s-${index}`,
      label: `会话 ${index}`,
      archived: index % 2 === 0,
      status: 'ready',
    })),
    activeSessionId: 's-0',
  }));
  assert.equal(parseSnapshot(full).sessions.length, 64);
  assert.equal(
    parseSnapshot({ ...full, sessions: [...full.sessions, { id: 's-64', label: 'X', archived: false, status: 'ready' }] }),
    null,
  );
});

test('撤权后会话目录与列表投影清空，不借目录授予正文或发送权', () => {
  const revoked = makeSnapshot(7, {
    revoked: true,
    ...sessionCatalog({ activeSessionId: null, sessions: [] }),
  });
  assert.equal(displaySessionCatalog(revoked), null);
  assert.deepEqual(displaySessions(revoked), []);
  assert.deepEqual(displaySessions(makeSnapshot(7, sessionCatalog())), sessionCatalog().sessions);
});

test('聊天草稿丢弃回执与按会话水位字段严格、无正文且修订一致', () => {
  assert.deepEqual(IPC_CONTRACT.chatDraftFields, ['revision', 'discard', 'sessions']);
  assert.deepEqual(IPC_CONTRACT.chatDraftDiscardFields, ['revision', 'sessionIds']);
  assert.deepEqual(IPC_CONTRACT.chatDraftSessionFields, ['id', 'revision', 'discardedRevision']);
  assert.ok(IPC_CONTRACT.snapshotFields.includes('chatDraft'));

  const receipt = {
    revision: 42,
    discard: { revision: 41, sessionIds: ['session-a', 'session-b'] },
    sessions: [
      { id: 'session-a', revision: 41, discardedRevision: 41 },
      { id: 'session-b', revision: 42, discardedRevision: null },
    ],
  };
  assert.deepEqual(parseSnapshot({ ...makeSnapshot(7), chatDraft: receipt }).chatDraft, receipt);
  // 缺少按会话水位时拒绝整份快照，不能回退到全局最新回执。
  assert.equal(
    parseSnapshot({
      ...makeSnapshot(7),
      chatDraft: { revision: 42, discard: null },
    }),
    null,
  );

  assert.equal(parseSnapshot({ ...makeSnapshot(7), chatDraft: undefined }), null);
  assert.equal(parseSnapshot({ ...makeSnapshot(7), chatDraft: { revision: 42 } }), null);
  assert.equal(
    parseSnapshot({ ...makeSnapshot(7), chatDraft: { revision: 42, discard: null, text: 'x' } }),
    null,
  );
  assert.equal(
    parseSnapshot({
      ...makeSnapshot(7),
      chatDraft: { revision: 42, discard: { revision: 41, sessionIds: ['session-a'], body: 'x' } },
    }),
    null,
  );
  // 回执修订不得超过当前草稿集合修订，且必须是安全整数。
  assert.equal(
    parseSnapshot({ ...makeSnapshot(7), chatDraft: { revision: 41, discard: { revision: 42, sessionIds: ['session-a'] } } }),
    null,
  );
  assert.equal(
    parseSnapshot({ ...makeSnapshot(7), chatDraft: { revision: 42, discard: { revision: 41.5, sessionIds: ['session-a'] } } }),
    null,
  );
  for (const sessionIds of [[], ['session-a', 'session-a'], ['bad id'], ['x'.repeat(65)], [7]]) {
    assert.equal(
      parseSnapshot({ ...makeSnapshot(7), chatDraft: { revision: 42, discard: { revision: 41, sessionIds } } }),
      null,
    );
  }
  const tooMany = Array.from({ length: 65 }, (_, index) => `s-${index}`);
  assert.equal(
    parseSnapshot({ ...makeSnapshot(7), chatDraft: { revision: 42, discard: { revision: 41, sessionIds: tooMany } } }),
    null,
  );

  // 会话水位：重复 ID、超限、非法修订、丢弃修订晚于变更修订、未知字段全部拒绝。
  const withSessions = (sessions) => ({
    ...makeSnapshot(7),
    chatDraft: { revision: 42, discard: null, sessions },
  });
  assert.equal(
    parseSnapshot(withSessions([
      { id: 'session-a', revision: 41, discardedRevision: null },
      { id: 'session-a', revision: 42, discardedRevision: null },
    ])),
    null,
  );
  assert.equal(
    parseSnapshot(withSessions(
      Array.from({ length: 65 }, (_, index) => ({
        id: `s-${index}`,
        revision: 1,
        discardedRevision: null,
      })),
    )),
    null,
  );
  for (const sessions of [
    [{ id: 'bad id', revision: 1, discardedRevision: null }],
    [{ id: 'session-a', revision: -1, discardedRevision: null }],
    [{ id: 'session-a', revision: 43, discardedRevision: null }],
    [{ id: 'session-a', revision: 1, discardedRevision: 2 }],
    [{ id: 'session-a', revision: 1, discardedRevision: null, body: 'x' }],
    [{ id: 'session-a', revision: 1 }],
  ]) {
    assert.equal(parseSnapshot(withSessions(sessions)), null);
  }

  // 撤权后不得保留引用旧目录的回执或会话水位。
  const revoked = makeSnapshot(8, {
    revoked: true,
    ...sessionCatalog({ activeSessionId: null, sessions: [] }),
  });
  assert.equal(parseSnapshot({ ...revoked, chatDraft: receipt }), null);
  assert.equal(
    parseSnapshot({
      ...revoked,
      chatDraft: { revision: 8, discard: null, sessions: receipt.sessions },
    }),
    null,
  );
});
