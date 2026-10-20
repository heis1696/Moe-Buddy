const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');
const { parseSnapshot } = require('../.test-build/snapshot.js');

// Executes the production MainView script with controlled IPC completion. This
// checks the real component logic (draft-first transitions, session scoping,
// Unknown isolation, archived read-only, revoke/unmount) without adding a
// production inspection API or claiming DOM/WebView2 verification.
//
// Incremental real-WebView2 DOM smoke plan for the next round (not run here and
// not accepted against the D-only baseline host): with the A host landed, start
// one isolated --data-dir host and assert via real form/button input:
// 1) session list renders the host-issued labels/revision and no old draft text;
// 2) typing then switching flushes set_draft for the old session before the
//    lifecycle command, and the new session's composer is empty;
// 3) switching back restores the old session's own draft, never B's text;
// 4) a synthetic send-unknown session shows the isolation entry; creating a new
//    session keeps the old one Unknown and never re-sends the old draft;
// 5) archiving the current session keeps it selectable, disables send, and
//    offers no unarchive/delete control;
// 6) a forced rule-0 conflicting lifecycle response keeps the draft and shows a
//    truthful host rejection; revoke clears the list and draft; unloading the
//    main window while a lifecycle reply is in flight writes nothing back.

function component() {
  const sourcePath = path.join(__dirname, '../src/app/MainView.vue');
  const source = fs.readFileSync(sourcePath, 'utf8').match(/<script setup lang="ts">([\s\S]*?)<\/script>/)[1];
  const script = ts.transpileModule(source + `
    const sessionDrafts = {
      recall: (id) => buddyDrafts.recallEntry(buddyTarget.expectedBuddyId, id)?.text ?? null,
      recallEntry: (id) => buddyDrafts.recallEntry(buddyTarget.expectedBuddyId, id),
      has: (id) => buddyDrafts.peek(buddyTarget.expectedBuddyId, id) !== null,
      get size() { return buddyDrafts.count(buddyTarget.expectedBuddyId); },
    };
    module.exports.probe = {
      applyResponse, handleDraftSettled, startNewSession, switchToSession,
      requestArchiveSession, confirmArchiveSession, send, onDraftInput,
      snapshot, snapshots, draft, commandLock, commandUnconfirmed,
      sendIsolationAllowed, canSend, canCompose, sendBlockade,
      activeSessionArchived, sessionCatalog, sessions, sessionDrafts,
      buddyDrafts, buddyTarget, buddyScopeGeneration,
      archiveArmed, newSessionBlockReason, switchSessionBlockReason,
      archiveBlockReason, activeSessionId, lastError, sessionsRevision,
      initialize,
    };
  `, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const calls = [];
  const beforeUnmount = [];
  let nextSnapshotValue = null;
  const scope = vue.effectScope();
  const exported = { exports: {} };
  const context = {
    module: exported,
    exports: exported.exports,
    setTimeout,
    clearTimeout,
    // 生产 initialize 会记录/清理监听释放标记；真实 DOM 由 native smoke 覆盖，
    // 这里只提供最小属性载体以走通同一初始化路径。
    document: { documentElement: { dataset: {} } },
    require(name) {
      if (name === 'vue') return {
        ...vue,
        onMounted() {},
        onBeforeUnmount(callback) { beforeUnmount.push(callback); },
      };
      if (name === '../platform/tauri') return {
        dispatchPrototype: (command, context) =>
          new Promise((resolve, reject) => { calls.push({ command, context, resolve, reject }); }),
        updateManagementDraftState: async (version, dirty) => ({ version, dirty }),
        invokeWindowCommand: async () => {},
        listenFrameEvents: async () => () => {},
        listenSnapshotEvents: async () => () => {},
        requestFrame: async () => ({ frameIndex: 0, paused: false, faulted: false }),
        requestSnapshot: async () => ({ snapshot: nextSnapshotValue, error: null }),
      };
      if (name.startsWith('../core/')) {
        return require(path.join(__dirname, '../.test-build', name.slice('../core/'.length) + '.js'));
      }
      throw new Error(`Unexpected component dependency: ${name}`);
    },
  };
  scope.run(() => vm.runInNewContext(script, context, { filename: sourcePath }));
  let unmounted = false;
  return {
    api: exported.exports.probe,
    calls,
    setNextSnapshot(value) { nextSnapshotValue = value; },
    unmount() {
      if (unmounted) return;
      unmounted = true;
      for (const callback of beforeUnmount) callback();
      scope.stop();
    },
  };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

function session(id, overrides = {}) {
  return { id, label: `会话 ${id}`, archived: false, status: 'ready', ...overrides };
}

function snapshot(revision, overrides = {}) {
  const base = {
    revision,
    presentationRevision: overrides.presentationRevision ?? revision,
    mode: 'offline',
    revoked: false,
    chat: { draft: '', messages: [], status: 'ready', failure: null },
    closeConfirmation: false,
    closeIntent: null,
    managementDraft: { version: 0, dirty: false, kind: 'none' },
    managementExit: null,
    managementDiscardedVersion: null,
    chatDraft: { revision: 1, discard: null },
    activePreset: '',
    externalTransmissionAllowed: false,
    presets: [],
    memories: [],
    diagnostics: [],
    notice: null,
    modelSettings: null,
    persistenceEnabled: true,
    activeSessionId: 's-a',
    sessionsRevision: 3,
    sessions: [session('s-a'), session('s-b')],
    sessionIsolationAllowed: false,
    // 冻结的 B 伙伴注册表三元组：严格解析要求它必填。
    activeBuddyId: 'buddy-a',
    buddiesRevision: 4,
    activeBuddyPersona: '伙伴 A 的合成人设',
    buddyCommit: null,
    buddies: [
      { id: 'buddy-a', label: '伙伴 A', profileRevision: 1, personaRevision: 1, modelRevision: 1 },
      { id: 'buddy-b', label: '伙伴 B', profileRevision: 1, personaRevision: 1, modelRevision: 1 },
    ],
  };
  const value = { ...base, ...overrides, chat: { ...base.chat, ...(overrides.chat ?? {}) } };
  // 撤权快照的伙伴注册表必须同时为空；冻结解析不接受撤权后仍带档案。
  if (value.revoked === true) {
    value.activeBuddyId = null;
    value.buddies = [];
    value.activeBuddyPersona = null;
  }
  // Earlier cases specify their draft revision once; the fixture supplies the
  // required per-session metadata. Production parsing has no legacy fallback.
  if (value.chatDraft.sessions === undefined) {
    const state = value.chatDraft;
    value.chatDraft = {
      ...state,
      sessions: value.revoked ? [] : value.sessions.map(({ id }) => ({
        id,
        revision: state.revision,
        discardedRevision: state.discard?.sessionIds.includes(id) ? state.discard.revision : null,
      })),
    };
  }
  return value;
}

function envelope(value, error = null) {
  return { snapshot: value, error };
}

function notice() {
  return { code: 'session_stale', message: 'Synthetic stale session', disposition: 'not_executed' };
}

async function load(instance, value) {
  instance.api.applyResponse(envelope(value), 'load');
  await tick();
}

async function typeDraft(instance, text) {
  instance.api.onDraftInput({ target: { value: text }, isComposing: false });
  await tick();
  return instance.calls[instance.calls.length - 1];
}

async function settleLast(instance, response) {
  const call = instance.calls[instance.calls.length - 1];
  call.resolve(response);
  await tick();
  return call;
}

test('发送 Unknown 后显式新建隔离会话：旧会话保持 Unknown、草稿不串会话、不自动重发', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    const draftCall = await typeDraft(instance, 'A 的旧草稿');
    assert.deepEqual(draftCall.command, {
      kind: 'set_draft',
      text: 'A 的旧草稿',
      expectedSessionId: 's-a',
      expectedSessionsRevision: 3,
    });
    await settleLast(instance, envelope(snapshot(6, { chat: { draft: 'A 的旧草稿' } })));

    const sending = instance.api.send();
    await tick();
    const sendCall = instance.calls[instance.calls.length - 1];
    assert.deepEqual(sendCall.command, {
      kind: 'send',
      expectedSessionId: 's-a',
      expectedSessionsRevision: 3,
    });
    sendCall.reject(new Error('synthetic ipc reply lost'));
    await sending;

    assert.equal(instance.api.commandUnconfirmed.value, true);
    // 前端不能按 send 操作名推定可另开；必须等应用快照给出隔离许可。
    assert.equal(instance.api.sendIsolationAllowed.value, false);
    assert.equal(instance.api.commandLock.locked, true);
    assert.equal(instance.api.canSend.value, false);

    // 宿主事件确认外部结果未知、本地处理结束且没有存储/撤权全局未知。
    instance.api.applyResponse(envelope(snapshot(7, {
      sessionIsolationAllowed: true,
      sessions: [session('s-a', { status: 'unknown' }), session('s-b')],
      chat: {
        draft: 'A 的旧草稿',
        status: 'unknown',
        failure: 'send_unknown',
      },
    })), 'load');
    await tick();
    assert.equal(instance.api.commandUnconfirmed.value, true, '普通快照不解锁旧请求');
    assert.equal(instance.api.commandLock.locked, true);
    assert.equal(instance.api.sendIsolationAllowed.value, true, '只有应用裁决的许可才开放隔离新建');
    assert.equal(instance.api.canSend.value, false);

    const creating = instance.api.startNewSession();
    await tick();
    const newCall = instance.calls[instance.calls.length - 1];
    assert.deepEqual(newCall.command, {
      kind: 'new_session',
      expectedSessionId: 's-a',
      expectedSessionsRevision: 3,
    });
    newCall.resolve(envelope(snapshot(8, {
      activeSessionId: 's-b',
      sessionsRevision: 4,
      sessions: [session('s-a', { status: 'unknown' }), session('s-b')],
      chat: { draft: '' },
    })));
    await creating;
    await tick();

    assert.equal(instance.api.activeSessionId.value, 's-b');
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.commandUnconfirmed.value, false);
    assert.equal(instance.api.commandLock.locked, false);
    assert.equal(
      instance.calls.filter((call) => call.command.kind === 'send').length,
      1,
      '旧发送 Unknown 不得自动重发',
    );
    assert.equal(instance.api.sessionDrafts.recall('s-a'), 'A 的旧草稿');

    const switching = instance.api.switchToSession('s-a');
    await tick();
    const switchCall = instance.calls[instance.calls.length - 1];
    assert.deepEqual(switchCall.command, {
      kind: 'switch_session',
      id: 's-a',
      expectedSessionId: 's-b',
      expectedSessionsRevision: 4,
    });
    switchCall.resolve(envelope(snapshot(9, {
      activeSessionId: 's-a',
      sessionsRevision: 5,
      sessions: [session('s-a', { status: 'unknown' }), session('s-b')],
      sessionIsolationAllowed: true,
      chat: {
        draft: 'A 的旧草稿',
        status: 'unknown',
        failure: 'send_unknown',
      },
    })));
    await switching;
    await tick();

    assert.equal(instance.api.activeSessionId.value, 's-a');
    assert.equal(instance.api.draft.value, 'A 的旧草稿');
    assert.equal(instance.api.commandLock.locked, true, '切回旧会话必须保留 Unknown');
    assert.equal(instance.api.sendIsolationAllowed.value, true);
    assert.equal(
      instance.calls.filter((call) => call.command.kind === 'send').length,
      1,
    );
  } finally {
    instance.unmount();
  }
});

test('先同步排队草稿再新建：旧会话草稿只带旧预期，不进入新会话', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    instance.api.onDraftInput({ target: { value: 'draft-one' }, isComposing: false });
    await tick();
    instance.api.onDraftInput({ target: { value: 'draft-two' }, isComposing: false });
    assert.equal(instance.calls.length, 1);

    const creating = instance.api.startNewSession();
    await tick();
    assert.equal(instance.calls.length, 1, '草稿未同步前不得请求生命周期动作');

    instance.calls[0].resolve(envelope(snapshot(6, { chat: { draft: 'draft-one' } })));
    await tick();
    assert.equal(instance.calls.length, 2);
    assert.deepEqual(instance.calls[1].command, {
      kind: 'set_draft',
      text: 'draft-two',
      expectedSessionId: 's-a',
      expectedSessionsRevision: 3,
    });
    assert.equal(
      instance.calls.some((call) => call.command.kind === 'new_session'),
      false,
    );

    instance.calls[1].resolve(envelope(snapshot(7, { chat: { draft: 'draft-two' } })));
    await tick();
    const newCall = instance.calls[2];
    assert.equal(newCall.command.kind, 'new_session');
    assert.equal(newCall.command.expectedSessionId, 's-a');
    newCall.resolve(envelope(snapshot(8, {
      activeSessionId: 's-b',
      sessionsRevision: 4,
      chat: { draft: '' },
    })));
    await creating;
    await tick();

    assert.equal(instance.api.activeSessionId.value, 's-b');
    assert.equal(instance.api.draft.value, '');
    assert.equal(
      instance.calls.filter(
        (call) => call.command.kind === 'set_draft' && call.command.expectedSessionId === 's-b',
      ).length,
      0,
      '旧草稿不得带新会话预期派发',
    );
    assert.equal(instance.api.sessionDrafts.recall('s-a'), 'draft-two');
  } finally {
    instance.unmount();
  }
});

test('切换后旧会话/旧修订的迟到草稿回包不得回填当前会话', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    const switching = instance.api.switchToSession('s-b');
    await tick();
    instance.calls[instance.calls.length - 1].resolve(envelope(snapshot(6, {
      activeSessionId: 's-b',
      sessionsRevision: 4,
      chat: { draft: 'B 草稿' },
    })));
    await switching;
    await tick();
    assert.equal(instance.api.activeSessionId.value, 's-b');
    assert.equal(instance.api.draft.value, 'B 草稿');

    // 旧 revision 直接走排序丢弃。
    instance.api.applyResponse(envelope(snapshot(4, {
      activeSessionId: 's-a',
      chat: { draft: 'A 迟到草稿' },
    })), 'draft');
    assert.equal(instance.api.activeSessionId.value, 's-b');
    assert.equal(instance.api.draft.value, 'B 草稿');

    // 新 revision 但属于另一会话的草稿回包，按会话复验拒绝回填。
    instance.api.handleDraftSettled('A 迟到草稿', envelope(snapshot(9, {
      activeSessionId: 's-a',
      chat: { draft: 'A 迟到草稿' },
    })), null);
    assert.equal(instance.api.activeSessionId.value, 's-b');
    assert.equal(instance.api.draft.value, 'B 草稿');
    assert.match(instance.api.lastError.value, /其他会话/);

    // 同一会话 ID 但目录修订较旧的回包同样不得回填草稿或选择。
    instance.api.handleDraftSettled('B 旧修订草稿', envelope(snapshot(10, {
      activeSessionId: 's-b',
      sessionsRevision: 3,
      chat: { draft: 'B 旧修订草稿' },
    })), null);
    assert.equal(instance.api.activeSessionId.value, 's-b');
    assert.equal(instance.api.draft.value, 'B 草稿');
    assert.match(instance.api.lastError.value, /较旧会话目录修订/);
  } finally {
    instance.unmount();
  }
});

test('切换失败保稿：拒绝回包不改当前会话，草稿仍在本地与输入框', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    await typeDraft(instance, 'keep me');
    await settleLast(instance, envelope(snapshot(6, { chat: { draft: 'keep me' } })));

    const switching = instance.api.switchToSession('s-b');
    await tick();
    const switchCall = instance.calls[instance.calls.length - 1];
    assert.equal(switchCall.command.kind, 'switch_session');
    switchCall.resolve({
      snapshot: snapshot(7, { chat: { draft: 'keep me' } }),
      error: notice(),
    });
    await switching;
    await tick();

    assert.equal(instance.api.activeSessionId.value, 's-a');
    assert.equal(instance.api.draft.value, 'keep me');
    assert.equal(instance.api.sessionDrafts.recall('s-a'), 'keep me');
    assert.match(instance.api.lastError.value, /未执行/);
    assert.equal(instance.api.commandLock.locked, false);
    assert.equal(
      instance.calls.filter(
        (call) => call.command.kind === 'set_draft' && call.command.expectedSessionId === 's-b',
      ).length,
      0,
    );
  } finally {
    instance.unmount();
  }
});

test('事件先到、切换回包后到且失败：新会话已生效时不把旧稿写回', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    await typeDraft(instance, 'A 待切换草稿');
    await settleLast(instance, envelope(snapshot(6, { chat: { draft: 'A 待切换草稿' } })));

    const switching = instance.api.switchToSession('s-b');
    await tick();
    const switchCall = instance.calls[instance.calls.length - 1];
    assert.equal(switchCall.command.kind, 'switch_session');

    // 事件先到：宿主已把当前会话切到 B，并带有 B 自己的草稿。
    instance.api.applyResponse(envelope(snapshot(7, {
      activeSessionId: 's-b',
      sessionsRevision: 4,
      chat: { draft: 'B 自己的草稿' },
    })), 'load');
    await tick();
    assert.equal(instance.api.activeSessionId.value, 's-b');
    assert.equal(instance.api.draft.value, 'B 自己的草稿');

    // 旧切换命令的失败回包后到：不得把 A 的旧稿写回 B 的输入框。
    switchCall.resolve({
      snapshot: snapshot(8, {
        activeSessionId: 's-b',
        sessionsRevision: 5,
        chat: { draft: 'B 自己的草稿' },
      }),
      error: notice(),
    });
    await switching;
    await tick();

    assert.equal(instance.api.activeSessionId.value, 's-b');
    assert.equal(instance.api.draft.value, 'B 自己的草稿');
    assert.match(instance.api.lastError.value, /未执行/);
    assert.equal(instance.api.sessionDrafts.recall('s-a'), 'A 待切换草稿');
  } finally {
    instance.unmount();
  }
});

test('撤权后生命周期失败回包不恢复旧稿或敏感缓存', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    await typeDraft(instance, '撤权前等待切换');
    await settleLast(instance, envelope(snapshot(6, { chat: { draft: '撤权前等待切换' } })));
    assert.equal(instance.api.sessionDrafts.size, 1);

    const switching = instance.api.switchToSession('s-b');
    await tick();
    const switchCall = instance.calls[instance.calls.length - 1];
    assert.equal(switchCall.command.kind, 'switch_session');

    instance.api.applyResponse(envelope(snapshot(7, {
      revoked: true,
      activeSessionId: null,
      sessionsRevision: 4,
      sessions: [],
      chat: { draft: '不应显示', messages: [{ role: 'user', text: '旧消息' }] },
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.sessionDrafts.size, 0);

    switchCall.resolve({
      snapshot: null,
      error: { code: 'session_unknown', message: 'Synthetic unknown', disposition: 'unknown' },
    });
    await switching;
    await tick();

    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.sessionDrafts.size, 0);
    assert.equal(instance.api.snapshot.value.revoked, true);
    assert.equal(instance.api.commandLock.locked, true, '撤权后的未知仍保守锁定');
  } finally {
    instance.unmount();
  }
});

test('归档当前会话：只读禁发、保留选择与草稿，不自动新建下一会话', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5, { chat: { draft: '归档前草稿' } }));
    assert.equal(instance.api.activeSessionArchived.value, false);

    instance.api.requestArchiveSession();
    assert.equal(instance.api.archiveArmed.value, true);
    const archiving = instance.api.confirmArchiveSession();
    await tick();
    const archiveCall = instance.calls[instance.calls.length - 1];
    assert.deepEqual(archiveCall.command, {
      kind: 'archive_session',
      id: 's-a',
      expectedSessionId: 's-a',
      expectedSessionsRevision: 3,
    });
    archiveCall.resolve(envelope(snapshot(6, {
      sessionsRevision: 4,
      chat: { draft: '归档前草稿' },
      sessions: [session('s-a', { archived: true }), session('s-b')],
    })));
    await archiving;
    await tick();

    assert.equal(instance.api.activeSessionArchived.value, true);
    assert.equal(instance.api.canCompose.value, false);
    assert.equal(instance.api.canSend.value, false);
    assert.equal(instance.api.draft.value, '归档前草稿');
    assert.match(instance.api.sendBlockade.value, /归档/);
    assert.equal(instance.api.archiveArmed.value, false);
    assert.equal(
      instance.calls.some((call) => call.command.kind === 'new_session'),
      false,
      '归档不得隐式创建下一会话',
    );

    await instance.api.send();
    await tick();
    assert.equal(
      instance.calls.some((call) => call.command.kind === 'send'),
      false,
      '归档会话不得派发发送',
    );

    // 仍可显式切换到其他会话；归档项不是删除。
    const switching = instance.api.switchToSession('s-b');
    await tick();
    const switchCall = instance.calls[instance.calls.length - 1];
    assert.equal(switchCall.command.kind, 'switch_session');
    switchCall.resolve(envelope(snapshot(7, {
      activeSessionId: 's-b',
      sessionsRevision: 5,
      sessions: [session('s-a', { archived: true }), session('s-b')],
    })));
    await switching;
    await tick();
    assert.equal(instance.api.activeSessionId.value, 's-b');
    assert.equal(instance.api.canCompose.value, true);

    // 归档后的目录修订用于下一次草稿命令，不沿用旧修订。
    const draftCall = await typeDraft(instance, 'B 新草稿');
    assert.equal(draftCall.command.expectedSessionId, 's-b');
    assert.equal(draftCall.command.expectedSessionsRevision, 5);
  } finally {
    instance.unmount();
  }
});

test('记忆/设置等全局 Unknown 不能被新建会话或普通刷新解除', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    instance.api.applyResponse({
      snapshot: snapshot(6, {
        // 即使会话仍显示发送未知，叠加记忆保存未知后应用也不会给出隔离许可。
        sessionIsolationAllowed: true,
        sessions: [session('s-a', { status: 'unknown' }), session('s-b')],
        chat: { status: 'unknown', failure: 'send_unknown' },
      }),
      error: { code: 'memory_io', message: 'Synthetic unknown', disposition: 'unknown' },
    }, 'remember');
    await tick();
    assert.equal(instance.api.commandUnconfirmed.value, true);
    assert.equal(instance.api.sendIsolationAllowed.value, false);
    assert.equal(instance.api.commandLock.hasGlobalUnknown, true);

    await instance.api.startNewSession();
    await tick();
    assert.equal(
      instance.calls.filter((call) => call.command.kind === 'new_session').length,
      0,
      '全局 Unknown 不得通过新建会话绕过',
    );
    assert.match(instance.api.lastError.value, /不能解除|未确认/);

    instance.api.commandLock.beginRefresh();
    instance.api.commandLock.finishRefresh(true, true);
    assert.equal(instance.api.commandLock.locked, true);
  } finally {
    instance.unmount();
  }
});

test('保存失败叠加 send_unknown 时应用不开放隔离新建，旧请求也不被普通快照解锁', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    instance.api.applyResponse(envelope(snapshot(6, {
      sessionIsolationAllowed: false,
      sessions: [session('s-a', { status: 'unknown' }), session('s-b')],
      chat: { status: 'unknown', failure: 'send_unknown' },
    })), 'load');
    await tick();
    assert.equal(instance.api.sendIsolationAllowed.value, false);
    assert.match(instance.api.newSessionBlockReason.value, /未开放|保存|隔离/);
    await instance.api.startNewSession();
    await tick();
    assert.equal(
      instance.calls.filter((call) => call.command.kind === 'new_session').length,
      0,
      '保存失败叠加 send_unknown 不得凭聊天状态另开会话',
    );

    // 普通成功快照即使变了 revision 也不清除旧会话的 Unknown。
    instance.api.applyResponse({
      snapshot: snapshot(7),
      error: { code: 'send_timeout', message: 'Synthetic unknown', disposition: 'unknown' },
    }, 'send');
    await tick();
    assert.equal(instance.api.commandLock.locked, true);
    assert.equal(instance.api.canSend.value, false);
    instance.api.applyResponse(envelope(snapshot(8, { sessionIsolationAllowed: false })), 'load');
    await tick();
    assert.equal(instance.api.commandLock.locked, true, '普通快照不得解锁旧请求');
  } finally {
    instance.unmount();
  }
});

test('撤权清空会话目录与各会话草稿，不保留可发送入口', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    await typeDraft(instance, '撤权前草稿');
    await settleLast(instance, envelope(snapshot(6, { chat: { draft: '撤权前草稿' } })));
    assert.equal(instance.api.sessionDrafts.size, 1);

    instance.api.applyResponse(envelope(snapshot(7, {
      revoked: true,
      activeSessionId: null,
      sessionsRevision: 4,
      sessions: [],
      chat: { draft: '不应显示', messages: [{ role: 'user', text: '旧消息' }], status: 'ready' },
    })), 'revoke');
    await tick();

    assert.equal(instance.api.draft.value, '');
    assert.deepEqual(instance.api.sessions.value, []);
    assert.equal(instance.api.sessionDrafts.size, 0);
    assert.equal(instance.api.canCompose.value, false);
    assert.equal(instance.api.activeSessionArchived.value, false);
  } finally {
    instance.unmount();
  }
});

test('卸载后生命周期迟到回包不写回快照或草稿', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    const creating = instance.api.startNewSession();
    await tick();
    const newCall = instance.calls[instance.calls.length - 1];
    assert.equal(newCall.command.kind, 'new_session');

    instance.unmount();
    newCall.resolve(envelope(snapshot(7, {
      activeSessionId: 's-b',
      sessionsRevision: 4,
      chat: { draft: '不应写回' },
    })));
    await creating;
    await tick();

    assert.equal(instance.api.snapshot.value, null);
    assert.equal(instance.api.snapshots.current, null);
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.sessionDrafts.size, 0);
  } finally {
    instance.unmount();
  }
});

test('缺会话三元组或隔离许可的旧快照被严格拒绝，不进入生命周期 UI', () => {
  assert.ok(parseSnapshot(snapshot(5)), 'strict A snapshot must parse');
  for (const field of [
    'activeSessionId',
    'sessionsRevision',
    'sessions',
    'sessionIsolationAllowed',
  ]) {
    const legacy = snapshot(5);
    delete legacy[field];
    assert.equal(parseSnapshot(legacy), null, `missing ${field} must be rejected`);
  }
  assert.equal(
    parseSnapshot({ ...snapshot(5), sessionIsolationAllowed: 'yes' }),
    null,
    'sessionIsolationAllowed must be a boolean',
  );
});

test('草稿同步期间会话变化时取消发送，不把旧预期带入新会话', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    const draftCall = await typeDraft(instance, 'A 待同步草稿');
    const sending = instance.api.send();
    await tick();
    assert.equal(
      instance.calls.some((call) => call.command.kind === 'send'),
      false,
      '草稿未同步前不得发送',
    );

    // 外部快照事件把当前会话切到 B。
    instance.api.applyResponse(envelope(snapshot(6, {
      activeSessionId: 's-b',
      sessionsRevision: 4,
      chat: { draft: 'B 草稿' },
    })), 'load');
    await tick();
    draftCall.resolve(envelope(snapshot(4, {
      activeSessionId: 's-a',
      chat: { draft: 'A 待同步草稿' },
    })));
    await sending;
    await tick();

    assert.equal(instance.api.activeSessionId.value, 's-b');
    assert.equal(instance.api.draft.value, 'B 草稿');
    assert.equal(
      instance.calls.some((call) => call.command.kind === 'send'),
      false,
      '会话已变化时不得派发发送',
    );
    assert.match(instance.api.lastError.value, /草稿尚未同步|会话在草稿同步期间变化/);
  } finally {
    instance.unmount();
  }
});

test('撤权后迟到的非撤权回包不能复活会话或草稿', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    instance.api.applyResponse(envelope(snapshot(7, {
      revoked: true,
      activeSessionId: null,
      sessionsRevision: 4,
      sessions: [],
      chat: { draft: '不应显示', messages: [], status: 'ready' },
    })), 'revoke');
    await tick();
    assert.equal(instance.api.snapshot.value.revoked, true);

    // 更高展示修订的迟到业务回包也不得复活撤权前状态。
    instance.api.applyResponse(envelope(snapshot(99, {
      activeSessionId: 's-a',
      sessionsRevision: 9,
      chat: { draft: '迟到草稿', messages: [{ role: 'assistant', text: '迟到回复' }] },
    })), 'send');
    await tick();

    assert.equal(instance.api.snapshot.value.revoked, true);
    assert.equal(instance.api.activeSessionId.value, null);
    assert.deepEqual(instance.api.sessions.value, []);
    assert.equal(instance.api.draft.value, '');
  } finally {
    instance.unmount();
  }
});

test('同 ID 但目录修订已推进的旧失败回包不回填旧稿或旧缓存', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    await typeDraft(instance, '旧稿');
    await settleLast(instance, envelope(snapshot(6, { chat: { draft: '旧稿' } })));

    const switching = instance.api.switchToSession('s-b');
    await tick();
    const switchCall = instance.calls[instance.calls.length - 1];
    assert.equal(switchCall.command.kind, 'switch_session');
    assert.equal(switchCall.command.expectedSessionsRevision, 3);

    // 目录修订推进但活动会话仍是 s-a（例如其他会话归档），宿主已给出新草稿基线。
    instance.api.applyResponse(envelope(snapshot(7, {
      sessionsRevision: 4,
      chat: { draft: '宿主新基线' },
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, '宿主新基线');

    // 同一会话 ID 但目录修订已变：旧请求的失败回包不得把旧稿写回。
    switchCall.resolve({
      snapshot: snapshot(8, { sessionsRevision: 4, chat: { draft: '宿主新基线' } }),
      error: notice(),
    });
    await switching;
    await tick();

    assert.equal(instance.api.activeSessionId.value, 's-a');
    assert.equal(instance.api.draft.value, '宿主新基线', '同 ID 新修订时不得回填旧稿');
    assert.match(instance.api.lastError.value, /未执行/);
  } finally {
    instance.unmount();
  }
});

test('A→B→A 回程后的新编辑不被原 A 旧请求失败覆盖', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    await typeDraft(instance, 'A 旧稿');
    await settleLast(instance, envelope(snapshot(6, { chat: { draft: 'A 旧稿' } })));

    const switching = instance.api.switchToSession('s-b');
    await tick();
    const switchCall = instance.calls[instance.calls.length - 1];
    assert.equal(switchCall.command.kind, 'switch_session');
    assert.equal(switchCall.command.expectedSessionsRevision, 3);

    // 宿主先切到 B，再从 B 切回 A：回程的 A 已是新目录修订。
    instance.api.applyResponse(envelope(snapshot(7, {
      activeSessionId: 's-b',
      sessionsRevision: 4,
      chat: { draft: 'B 草稿' },
    })), 'load');
    await tick();
    instance.api.applyResponse(envelope(snapshot(8, {
      activeSessionId: 's-a',
      sessionsRevision: 5,
      chat: { draft: 'A 回程草稿' },
    })), 'load');
    await tick();
    assert.equal(instance.api.activeSessionId.value, 's-a');
    assert.equal(instance.api.sessionsRevision.value, 5);

    // 回程后用户继续编辑；旧请求（派发时 rev 3）的失败不得覆盖它。
    instance.api.onDraftInput({ target: { value: 'A 回程新编辑' }, isComposing: false });
    await tick();
    switchCall.resolve({
      snapshot: snapshot(9, {
        activeSessionId: 's-a',
        sessionsRevision: 5,
        chat: { draft: 'A 回程新编辑' },
      }),
      error: notice(),
    });
    await switching;
    await tick();

    assert.equal(instance.api.draft.value, 'A 回程新编辑');
    assert.equal(instance.api.sessionDrafts.recall('s-a'), 'A 回程新编辑');
    assert.match(instance.api.lastError.value, /未执行/);
  } finally {
    instance.unmount();
  }
});

test('派发后新编辑时原请求失败/抛错都不回填旧稿，且抛错仍保持 Unknown 锁', async () => {
  // 失败回包分支。
  const failed = component();
  try {
    await load(failed, snapshot(5));
    await typeDraft(failed, '旧稿');
    await settleLast(failed, envelope(snapshot(6, { chat: { draft: '旧稿' } })));

    const switching = failed.api.switchToSession('s-b');
    await tick();
    const switchCall = failed.calls[failed.calls.length - 1];
    assert.equal(switchCall.command.kind, 'switch_session');

    failed.api.onDraftInput({ target: { value: '新编辑' }, isComposing: false });
    await tick();
    assert.equal(failed.api.draft.value, '新编辑');

    switchCall.resolve({
      snapshot: snapshot(7, { chat: { draft: '新编辑' } }),
      error: notice(),
    });
    await switching;
    await tick();

    assert.equal(failed.api.draft.value, '新编辑', '派发后的新编辑不得被旧失败回包覆盖');
    assert.equal(failed.api.sessionDrafts.recall('s-a'), '新编辑');
  } finally {
    failed.unmount();
  }

  // 抛错（回包丢失）分支：保守锁定 Unknown，但仍不得回填旧稿。
  const thrown = component();
  try {
    await load(thrown, snapshot(5));
    await typeDraft(thrown, '旧稿');
    await settleLast(thrown, envelope(snapshot(6, { chat: { draft: '旧稿' } })));

    const switching = thrown.api.switchToSession('s-b');
    await tick();
    const switchCall = thrown.calls[thrown.calls.length - 1];
    assert.equal(switchCall.command.kind, 'switch_session');

    thrown.api.onDraftInput({ target: { value: '抛错前新编辑' }, isComposing: false });
    await tick();
    switchCall.reject(new Error('synthetic lifecycle reply lost'));
    await switching;
    await tick();

    assert.equal(thrown.api.draft.value, '抛错前新编辑');
    assert.equal(thrown.api.sessionDrafts.recall('s-a'), '抛错前新编辑');
    assert.equal(thrown.api.commandUnconfirmed.value, true, '回包丢失仍须保持 Unknown 锁');
    assert.equal(thrown.api.commandLock.locked, true);
    assert.equal(thrown.api.canSend.value, false);
  } finally {
    thrown.unmount();
  }
});

function discardReceipt(revision, sessionIds) {
  return { revision, discard: { revision, sessionIds } };
}

test('CloseChat 明确丢弃回执后，重读同会话不复活旧稿也不自动派发 set_draft', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    await typeDraft(instance, 'A 待丢弃草稿');
    await settleLast(instance, envelope(snapshot(6, {
      chat: { draft: 'A 待丢弃草稿' },
      chatDraft: { revision: 2, discard: null },
    })));
    assert.equal(instance.api.draft.value, 'A 待丢弃草稿');
    assert.equal(instance.api.sessionDrafts.has('s-a'), true);

    // 宿主确认丢弃：事件先到（带权威回执与空稿），命令回包模拟丢失。
    instance.api.applyResponse(envelope(snapshot(7, {
      chat: { draft: '' },
      closeConfirmation: false,
      chatDraft: discardReceipt(4, ['s-a']),
    })), 'confirm_close');
    await tick();
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.sessionDrafts.has('s-a'), false, 'authoritative receipt must clear the local cache');

    // “重新读取状态”走生产 initialize：draftTarget 被重置后不得从本地缓存复活旧稿。
    instance.setNextSnapshot(snapshot(8, {
      chat: { draft: '' },
      chatDraft: discardReceipt(4, ['s-a']),
    }));
    await instance.api.initialize();
    await tick();
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.sessionDrafts.size, 0);
    assert.equal(
      instance.calls.filter((call) => call.command.kind === 'set_draft').length,
      1,
      're-reading after an explicit discard must not auto-dispatch set_draft',
    );
  } finally {
    instance.unmount();
  }
});

test('丢弃回执按修订幂等：事件/回包任一先到只清被丢弃会话，A→B→A 不复活', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    await typeDraft(instance, 'A 待丢弃');
    await settleLast(instance, envelope(snapshot(6, {
      chat: { draft: 'A 待丢弃' },
      chatDraft: { revision: 2, discard: null },
    })));

    // 在 B 也留下自己的草稿，随后切回 A。
    const toB = instance.api.switchToSession('s-b');
    await tick();
    instance.calls[instance.calls.length - 1].resolve(envelope(snapshot(7, {
      activeSessionId: 's-b',
      sessionsRevision: 4,
      chat: { draft: '' },
      chatDraft: { revision: 2, discard: null },
    })));
    await toB;
    await tick();
    await typeDraft(instance, 'B 保留');
    await settleLast(instance, envelope(snapshot(8, {
      activeSessionId: 's-b',
      sessionsRevision: 4,
      chat: { draft: 'B 保留' },
      chatDraft: { revision: 3, discard: null },
    })));
    const toA = instance.api.switchToSession('s-a');
    await tick();
    instance.calls[instance.calls.length - 1].resolve(envelope(snapshot(9, {
      activeSessionId: 's-a',
      sessionsRevision: 5,
      chat: { draft: 'A 待丢弃' },
      chatDraft: { revision: 3, discard: null },
    })));
    await toA;
    await tick();
    assert.equal(instance.api.draft.value, 'A 待丢弃');
    assert.equal(instance.api.sessionDrafts.recall('s-b'), 'B 保留');

    const setDraftCalls = () =>
      instance.calls.filter((call) => call.command.kind === 'set_draft').length;
    const before = setDraftCalls();
    instance.api.applyResponse(envelope(snapshot(10, {
      chat: { draft: '' },
      chatDraft: discardReceipt(5, ['s-a']),
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.sessionDrafts.has('s-a'), false);
    assert.equal(instance.api.sessionDrafts.recall('s-b'), 'B 保留', 'other session draft must survive');

    // 同回执的命令响应后到（修订更高、回执版本相同）：幂等，不重复清稿或清缓存。
    instance.api.applyResponse(envelope(snapshot(11, {
      chat: { draft: '' },
      chatDraft: discardReceipt(5, ['s-a']),
    })), 'confirm_close');
    await tick();
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.sessionDrafts.has('s-a'), false);
    assert.equal(setDraftCalls(), before, 'duplicate receipt must not dispatch another set_draft');

    // A→B：B 自己的草稿恢复；B→A：已丢弃的 A 草稿不得复活，也不自动回写。
    const againB = instance.api.switchToSession('s-b');
    await tick();
    instance.calls[instance.calls.length - 1].resolve(envelope(snapshot(12, {
      activeSessionId: 's-b',
      sessionsRevision: 6,
      chat: { draft: 'B 保留' },
      chatDraft: discardReceipt(5, ['s-a']),
    })));
    await againB;
    await tick();
    assert.equal(instance.api.draft.value, 'B 保留');
    const afterB = instance.calls.length;
    const againA = instance.api.switchToSession('s-a');
    await tick();
    instance.calls[instance.calls.length - 1].resolve(envelope(snapshot(13, {
      activeSessionId: 's-a',
      sessionsRevision: 7,
      chat: { draft: '' },
      chatDraft: discardReceipt(5, ['s-a']),
    })));
    await againA;
    await tick();
    assert.equal(instance.api.draft.value, '', 'A discarded draft must not come back through A→B→A');
    assert.equal(
      instance.calls.slice(afterB).filter(
        (call) => call.command.kind === 'set_draft' && call.command.expectedSessionId === 's-a',
      ).length,
      0,
      'A→B→A must not auto-sync the discarded draft back into A',
    );
  } finally {
    instance.unmount();
  }
});

test('旧丢弃回执与更新编辑：保留未同步新正文，不清掉更新编辑', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    await typeDraft(instance, '旧稿');
    await settleLast(instance, envelope(snapshot(6, {
      chat: { draft: '旧稿' },
      chatDraft: { revision: 2, discard: null },
    })));

    instance.api.applyResponse(envelope(snapshot(7, {
      chat: { draft: '' },
      chatDraft: discardReceipt(3, ['s-a']),
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, '');

    // 丢弃后的新编辑尚未同步：旧回执与“回执是最新修订”都不能清掉它。
    instance.api.onDraftInput({ target: { value: '新编辑' }, isComposing: false });
    await tick();
    const newEdit = instance.calls[instance.calls.length - 1];
    assert.equal(newEdit.command.kind, 'set_draft');
    instance.api.applyResponse(envelope(snapshot(8, {
      chat: { draft: '新编辑' },
      chatDraft: { revision: 4, discard: { revision: 3, sessionIds: ['s-a'] } },
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, '新编辑', 'a historical receipt must not clear newer text');
    instance.api.applyResponse(envelope(snapshot(9, {
      chat: { draft: '' },
      chatDraft: discardReceipt(4, ['s-a']),
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, '新编辑', 'unsynced newer text must survive an incoming discard');

    newEdit.resolve(envelope(snapshot(10, {
      chat: { draft: '新编辑' },
      chatDraft: discardReceipt(4, ['s-a']),
    })));
    await tick();
    assert.equal(instance.api.draft.value, '新编辑');
    assert.equal(
      instance.api.sessionDrafts.recall('s-a'),
      '新编辑',
      'a later user edit of the same session remains the local working copy',
    );
  } finally {
    instance.unmount();
  }
});

test('普通空快照、隐藏与关闭取消都不算丢弃，缓存保留且重开可恢复', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    await typeDraft(instance, '保留草稿');
    await settleLast(instance, envelope(snapshot(6, {
      chat: { draft: '保留草稿' },
      chatDraft: { revision: 2, discard: null },
    })));

    // 普通空快照（例如视图隐藏后清空展示缓存）不是丢弃：会话缓存必须保留。
    instance.api.applyResponse(envelope(snapshot(7, {
      chat: { draft: '' },
      chatDraft: { revision: 2, discard: null },
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.sessionDrafts.has('s-a'), true, 'plain empty snapshot must not clear the session cache');

    // 重读/重开按会话自己的缓存恢复草稿；宿主也有同一正文时不重复派发。
    instance.setNextSnapshot(snapshot(8, {
      chat: { draft: '保留草稿' },
      chatDraft: { revision: 2, discard: null },
    }));
    await instance.api.initialize();
    await tick();
    assert.equal(instance.api.draft.value, '保留草稿');

    // 取消关闭确认同样不是丢弃：草稿与缓存都保留。
    instance.api.applyResponse(envelope(snapshot(9, {
      chat: { draft: '保留草稿' },
      closeConfirmation: false,
      chatDraft: { revision: 2, discard: null },
    })), 'confirm_close');
    await tick();
    assert.equal(instance.api.draft.value, '保留草稿');
    assert.equal(instance.api.sessionDrafts.has('s-a'), true);
  } finally {
    instance.unmount();
  }
});

test('会话级丢弃水位：其他会话编辑不使 A 失效，漏收中间丢弃仍按会话补清', async () => {
  const instance = component();
  try {
    const sessions = (revA, discardedA, revB, discardedB) => [
      { id: 's-a', revision: revA, discardedRevision: discardedA },
      { id: 's-b', revision: revB, discardedRevision: discardedB },
    ];
    await load(instance, snapshot(5, {
      chatDraft: { revision: 1, discard: null, sessions: sessions(1, null, 1, null) },
    }));
    await typeDraft(instance, 'A 待丢弃');
    await settleLast(instance, envelope(snapshot(6, {
      chat: { draft: 'A 待丢弃' },
      chatDraft: { revision: 2, discard: null, sessions: sessions(2, null, 1, null) },
    })));

    // B 在 A 之后编辑，推进全局修订。
    const toB = instance.api.switchToSession('s-b');
    await tick();
    instance.calls[instance.calls.length - 1].resolve(envelope(snapshot(7, {
      activeSessionId: 's-b',
      sessionsRevision: 4,
      chat: { draft: '' },
      chatDraft: { revision: 2, discard: null, sessions: sessions(2, null, 1, null) },
    })));
    await toB;
    await tick();
    await typeDraft(instance, 'B 草稿');
    await settleLast(instance, envelope(snapshot(8, {
      activeSessionId: 's-b',
      sessionsRevision: 4,
      chat: { draft: 'B 草稿' },
      chatDraft: { revision: 3, discard: null, sessions: sessions(2, null, 3, null) },
    })));
    const toA = instance.api.switchToSession('s-a');
    await tick();
    instance.calls[instance.calls.length - 1].resolve(envelope(snapshot(9, {
      activeSessionId: 's-a',
      sessionsRevision: 5,
      chat: { draft: 'A 待丢弃' },
      chatDraft: { revision: 3, discard: null, sessions: sessions(2, null, 3, null) },
    })));
    await toA;
    await tick();
    assert.equal(instance.api.draft.value, 'A 待丢弃');

    const setDraftCalls = () =>
      instance.calls.filter((call) => call.command.kind === 'set_draft').length;
    const before = setDraftCalls();
    // 前端漏收了 A 在修订 3 的丢弃事件，只收到 B 在修订 5 的丢弃；最终
    // 快照仍带 A 的会话水位 3，必须据此清掉 A，而不是只看全局回执/修订。
    instance.api.applyResponse(envelope(snapshot(10, {
      activeSessionId: 's-a',
      sessionsRevision: 6,
      chat: { draft: '' },
      chatDraft: {
        revision: 5,
        discard: { revision: 5, sessionIds: ['s-b'] },
        sessions: sessions(3, 3, 5, 5),
      },
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.sessionDrafts.has('s-a'), false, 'A watermark 3 must still clear A cache');
    assert.equal(instance.api.sessionDrafts.has('s-b'), false, 'B discard watermark 5 clears B cache');
    assert.equal(setDraftCalls(), before, 'watermark consumption must not dispatch set_draft');
  } finally {
    instance.unmount();
  }
});

test('同会话丢弃后已确认的新编辑保留，旧同步回包不复活被丢弃正文', async () => {
  const instance = component();
  try {
    const session = (revision, discardedRevision) => [
      { id: 's-a', revision, discardedRevision },
    ];
    await load(instance, snapshot(5, {
      chatDraft: { revision: 1, discard: null, sessions: session(1, null) },
    }));
    await typeDraft(instance, 'A 旧稿');
    await settleLast(instance, envelope(snapshot(6, {
      chat: { draft: 'A 旧稿' },
      chatDraft: { revision: 2, discard: null, sessions: session(2, null) },
    })));

    instance.api.applyResponse(envelope(snapshot(7, {
      chat: { draft: '' },
      chatDraft: {
        revision: 3,
        discard: { revision: 3, sessionIds: ['s-a'] },
        sessions: session(3, 3),
      },
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.sessionDrafts.has('s-a'), false);

    // 丢弃后的新编辑被宿主确认：会话修订 4 晚于丢弃水位 3，必须保留。
    await typeDraft(instance, 'A 新稿');
    await settleLast(instance, envelope(snapshot(8, {
      chat: { draft: 'A 新稿' },
      chatDraft: {
        revision: 4,
        discard: { revision: 3, sessionIds: ['s-a'] },
        sessions: session(4, 3),
      },
    })));
    assert.equal(instance.api.draft.value, 'A 新稿');
    const cached = instance.api.sessionDrafts.recallEntry('s-a');
    assert.equal(cached?.text, 'A 新稿');
    assert.equal(cached?.revision, 4);
    assert.equal(cached?.synced, true);

    // 迟到的旧丢弃快照不能清掉晚于水位的已确认编辑。
    instance.api.applyResponse(envelope(snapshot(9, {
      chat: { draft: 'A 新稿' },
      chatDraft: {
        revision: 4,
        discard: { revision: 3, sessionIds: ['s-a'] },
        sessions: session(4, 3),
      },
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, 'A 新稿');

    // 旧队列/旧同步回包在丢弃后到达：整份旧快照被排序拒绝，不复活正文。
    instance.api.handleDraftSettled(
      'A 丢弃前在途',
      envelope(snapshot(4, {
        presentationRevision: 4,
        chat: { draft: 'A 丢弃前在途' },
        chatDraft: { revision: 5, discard: null, sessions: session(5, 3) },
      })),
      null,
    );
    await tick();
    assert.equal(instance.api.draft.value, 'A 新稿', 'an old reply must not resurrect earlier text');
    assert.equal(instance.api.sessionDrafts.recall('s-a'), 'A 新稿');
    assert.equal(
      instance.calls.filter(
        (call) => call.command.kind === 'set_draft' && call.command.text === 'A 丢弃前在途',
      ).length,
      0,
      'an old reply must not dispatch set_draft',
    );
  } finally {
    instance.unmount();
  }
});

test('卸载会清空逐会话草稿缓存与消费水位，迟到回包不再写回', async () => {
  const instance = component();
  await load(instance, snapshot(5, {
    chatDraft: {
      revision: 1,
      discard: null,
      sessions: [{ id: 's-a', revision: 1, discardedRevision: null }],
    },
  }));
  await typeDraft(instance, '卸载前草稿');
  await settleLast(instance, envelope(snapshot(6, {
    chat: { draft: '卸载前草稿' },
    chatDraft: {
      revision: 2,
      discard: null,
      sessions: [{ id: 's-a', revision: 2, discardedRevision: null }],
    },
  })));
  assert.equal(instance.api.sessionDrafts.size, 1);
  instance.unmount();
  assert.equal(instance.api.sessionDrafts.size, 0, 'unmount clears every session draft cache');
  instance.api.applyResponse(envelope(snapshot(7, {
    chat: { draft: '卸载前草稿' },
    chatDraft: {
      revision: 2,
      discard: null,
      sessions: [{ id: 's-a', revision: 2, discardedRevision: null }],
    },
  })), 'load');
  assert.equal(instance.api.snapshot.value, null, 'late responses must not rebuild state after unmount');
  assert.equal(instance.api.draft.value, '卸载前草稿', 'unmount keeps the last visible text without writing back');
});

test('撤权后丢弃回执不再清缓存，迟到的非撤权快照不能复活旧会话', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(5));
    await typeDraft(instance, '撤权前草稿');
    await settleLast(instance, envelope(snapshot(6, {
      chat: { draft: '撤权前草稿' },
      chatDraft: { revision: 2, discard: null },
    })));
    assert.equal(instance.api.sessionDrafts.size, 1);

    instance.api.applyResponse(envelope(snapshot(7, {
      revoked: true,
      activeSessionId: null,
      sessions: [],
      chat: { draft: '' },
      chatDraft: { revision: 3, discard: null },
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.sessionDrafts.size, 0, 'revocation clears every session draft cache');

    instance.api.applyResponse(envelope(snapshot(8, {
      activeSessionId: 's-a',
      sessionsRevision: 6,
      chat: { draft: '' },
      chatDraft: discardReceipt(4, ['s-a']),
    })), 'load');
    await tick();
    assert.equal(instance.api.draft.value, '', 'late non-revoked snapshots cannot resurrect a session');
    assert.equal(instance.api.sessionDrafts.size, 0);
  } finally {
    instance.unmount();
  }
});
