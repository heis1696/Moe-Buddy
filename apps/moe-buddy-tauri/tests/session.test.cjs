const test = require('node:test');
const assert = require('node:assert/strict');

const {
  SessionDraftStore,
  activeSessionOf,
  isArchivedActiveSession,
  isSafeSessionId,
  parseSessionCatalog,
  sessionCatalogOf,
  sessionExpectation,
  sessionStatusBlocksSend,
} = require('../.test-build/session.js');

function session(id, overrides = {}) {
  return { id, label: `L-${id}`, archived: false, status: 'ready', ...overrides };
}

function catalog(overrides = {}) {
  return {
    activeSessionId: 's-a',
    sessionsRevision: 2,
    sessions: [session('s-a'), session('s-b', { archived: true, status: 'unknown' })],
    ...overrides,
  };
}

test('会话 ID：1..64 字节可打印 ASCII，拒绝空、空白、非 ASCII 与超长', () => {
  assert.equal(isSafeSessionId('session-a'), true);
  assert.equal(isSafeSessionId('a'.repeat(64)), true);
  assert.equal(isSafeSessionId('a'.repeat(65)), false);
  assert.equal(isSafeSessionId(''), false);
  assert.equal(isSafeSessionId('has space'), false);
  assert.equal(isSafeSessionId(' tab\t'), false);
  assert.equal(isSafeSessionId('会话'), false);
  assert.equal(isSafeSessionId(1), false);
  assert.equal(isSafeSessionId(null), false);
});

test('目录三元组是必填冻结字段，缺任一项都拒绝而不是降级', () => {
  assert.equal(parseSessionCatalog({}, false), null);
  assert.equal(parseSessionCatalog({ activeSessionId: 's-a' }, false), null);
  assert.equal(sessionCatalogOf(null), null);
  assert.equal(sessionCatalogOf({}), null);
  for (const field of ['activeSessionId', 'sessionsRevision', 'sessions']) {
    const partial = { ...catalog() };
    delete partial[field];
    assert.equal(parseSessionCatalog(partial, false), null, field);
  }
  assert.deepEqual(sessionExpectation(null), {
    expectedSessionId: null,
    expectedSessionsRevision: 0,
  });
});

test('目录严格校验字段、上限、唯一 ID 与当前选择', () => {
  assert.deepEqual(parseSessionCatalog(catalog(), false), catalog());
  assert.equal(
    parseSessionCatalog(catalog({ sessions: [...catalog().sessions, session('s-a')] }), false),
    null,
  );
  assert.equal(parseSessionCatalog(catalog({ activeSessionId: 'missing' }), false), null);
  assert.equal(parseSessionCatalog(catalog({ activeSessionId: null }), false), null);
  assert.equal(
    parseSessionCatalog(catalog({ sessionsRevision: Number.MAX_SAFE_INTEGER + 1 }), false),
    null,
  );
  assert.equal(
    parseSessionCatalog(
      catalog({ sessions: [{ ...session('s-a'), status: 'paused' }] }),
      false,
    ),
    null,
  );
  assert.equal(
    parseSessionCatalog(catalog({ sessions: [{ ...session('s-a'), label: 1 }] }), false),
    null,
  );
  assert.equal(
    parseSessionCatalog(catalog({ sessions: [{ ...session('s-a'), extra: true }] }), false),
    null,
  );
  assert.equal(
    parseSessionCatalog(
      catalog({ sessions: Array.from({ length: 65 }, (_, index) => session(`s-${index}`)), activeSessionId: 's-0' }),
      false,
    ),
    null,
  );
});

test('撤权目录必须为空且无当前会话', () => {
  assert.deepEqual(
    parseSessionCatalog(catalog({ activeSessionId: null, sessions: [] }), true),
    { activeSessionId: null, sessionsRevision: 2, sessions: [] },
  );
  assert.equal(parseSessionCatalog(catalog(), true), null);
  assert.equal(parseSessionCatalog(catalog({ activeSessionId: null }), true), null);
});

test('当前会话预期、归档只读与目录发送门禁', () => {
  const snapshot = catalog({
    sessions: [
      session('s-a', { archived: true, status: 'ready' }),
      session('s-b', { status: 'sending' }),
    ],
  });
  assert.deepEqual(sessionExpectation(snapshot), {
    expectedSessionId: 's-a',
    expectedSessionsRevision: 2,
  });
  assert.equal(activeSessionOf(snapshot).id, 's-a');
  assert.equal(isArchivedActiveSession(snapshot), true);
  assert.equal(sessionStatusBlocksSend(snapshot), false);
  assert.equal(
    sessionStatusBlocksSend({ ...snapshot, activeSessionId: 's-b' }),
    true,
  );
  assert.equal(isArchivedActiveSession({}), false);
  assert.equal(sessionStatusBlocksSend(null), false);
});

test('按会话草稿有界保留、互不搬运，撤权/卸载可显式清空', () => {
  const store = new SessionDraftStore(2);
  store.remember('s-a', 'A 草稿');
  store.remember('s-b', 'B 草稿');
  assert.equal(store.recall('s-a'), 'A 草稿');
  assert.equal(store.recall('s-b'), 'B 草稿');
  assert.equal(store.size, 2);

  // 超过上限淘汰最久未使用项；最近的 recall 顺序决定 a 先被淘汰。
  store.remember('s-c', 'C 草稿');
  assert.equal(store.size, 2);
  assert.equal(store.has('s-a'), false);
  assert.equal(store.recall('s-b'), 'B 草稿');
  assert.equal(store.recall('s-c'), 'C 草稿');

  store.remember('bad id', '不合法');
  store.remember('s-d', 'D 草稿');
  assert.equal(store.has('bad id'), false);

  store.forget('s-c');
  assert.equal(store.recall('s-c'), null);
  store.clear();
  assert.equal(store.size, 0);
  assert.equal(store.recall('s-a'), null);
});

test('单条超长草稿不另存副本，避免按会话累积无界正文', () => {
  const store = new SessionDraftStore(4);
  store.remember('s-a', 'a'.repeat(8192));
  assert.equal(store.has('s-a'), true);
  store.remember('s-b', 'b'.repeat(8193));
  assert.equal(store.has('s-b'), false);
  assert.equal(store.recall('s-b'), null);
});
