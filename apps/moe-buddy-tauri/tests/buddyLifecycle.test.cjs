const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');

// 用真实生产 MainView 脚本和受控 IPC 完成，验证 B 伙伴作用域：创建/切换/改名、
// 同局部会话 ID 隔离、A→B→A、迟到模型回包、未保存编辑阻断与撤权清场。
// 不新增生产检查接口，也不替代 native smoke 的真实 WebView2 验收。
function component(options = {}) {
  const sourcePath = path.join(__dirname, '../src/app/MainView.vue');
  const source = fs.readFileSync(sourcePath, 'utf8').match(/<script setup lang="ts">([\s\S]*?)<\/script>/)[1];
  const script = ts.transpileModule(source + `
    module.exports.probe = {
      applyResponse, onDraftInput, snapshot, snapshots, draft,
      buddyDrafts, buddyDiscardWatermarks, activeBuddy, activeBuddyId, buddies,
      buddiesRevision, buddyActionBlockReason, createBuddyBlockReason, buddyError,
      buddyNotice, buddyCreateOpen, buddyRenameOpen, buddyForm, buddyRenameLabel,
      openBuddyCreate, cancelBuddyCreate, submitBuddyCreate,
      openBuddyRename, cancelBuddyRename, submitBuddyRename, switchToBuddy,
      hideChat, requestQuit, interactionLocked,
      activePanel, modelForm, modelSettings, applyModelSettings, lastError,
      modelApplyNotice, commandLock, syncModelForm, canSend, sendBlockade,
      modelPanelDirty, managementSync, noteModelInteraction,
      rememberDraft, rememberMemory, memoryNotice,
      get buddyTarget() { return buddyTarget; },
      get buddyScopeGeneration() { return buddyScopeGeneration; },
    };
  `, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  const calls = [];
  const managementCalls = [];
  const windowCalls = [];
  const beforeUnmount = [];
  const scope = vue.effectScope();
  const exported = { exports: {} };
  const context = {
    module: exported,
    exports: exported.exports,
    setTimeout,
    clearTimeout,
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
        updateManagementDraftState: async (version, dirty, kind, context) => {
          if (options.deferManagement === true) {
            // 受控延迟：让测试显式制造“第一份在途、第二份排队”的竞态。
            return new Promise((resolve) => {
              managementCalls.push({ version, dirty, kind, context, resolve });
            });
          }
          managementCalls.push({ version, dirty, kind, context });
          return { version, dirty };
        },
        invokeWindowCommand: (command, expectation) => {
          if (options.rejectWindow) {
            windowCalls.push({ command, expectation });
            return Promise.reject(new Error('synthetic lost reply'));
          }
          if (options.deferWindow === true) {
            return new Promise((resolve) => {
              windowCalls.push({ command, expectation, resolve });
            });
          }
          windowCalls.push({ command, expectation });
          return Promise.resolve();
        },
        listenFrameEvents: async () => () => {},
        listenSnapshotEvents: async () => () => {},
        requestFrame: async () => ({ frameIndex: 0, paused: false, faulted: false }),
        requestSnapshot: async () => ({ snapshot: null, error: null }),
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
    managementCalls,
    windowCalls,
    lastCall() { return calls[calls.length - 1]; },
    unmount() {
      if (unmounted) return;
      unmounted = true;
      for (const callback of beforeUnmount) callback();
      scope.stop();
    },
  };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));
const settle = async (instance, response) => {
  const call = instance.lastCall();
  call.resolve(response);
  await tick();
  await tick();
  return call;
};
const load = async (instance, value) => {
  instance.api.applyResponse({ snapshot: value, error: null }, 'load');
  await tick();
};

function buddyView(id, label, overrides = {}) {
  return {
    id,
    label,
    profileRevision: 1,
    personaRevision: 1,
    modelRevision: 1,
    ...overrides,
  };
}

const BUDDIES = [buddyView('b-a', '伙伴 A'), buddyView('b-b', '伙伴 B')];

function session(id = 's-shared', label = '共享会话') {
  return { id, label, archived: false, status: 'ready' };
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
    chatDraft: { revision: 1, discard: null, sessions: [] },
    activePreset: '',
    externalTransmissionAllowed: false,
    presets: [],
    memories: [],
    diagnostics: [],
    notice: null,
    modelSettings: null,
    persistenceEnabled: true,
    activeSessionId: 's-shared',
    sessionsRevision: 3,
    sessions: [session()],
    sessionIsolationAllowed: false,
    activeBuddyId: 'b-a',
    buddiesRevision: 4,
    activeBuddyPersona: '伙伴 A 的合成人设',
    buddyCommit: null,
    buddies: BUDDIES,
  };
  const value = {
    ...base,
    ...overrides,
    buddies: overrides.buddies ?? BUDDIES,
    chat: { ...base.chat, ...(overrides.chat ?? {}) },
  };
  if (value.revoked === true) {
    value.activeBuddyId = null;
    value.buddies = [];
    value.activeBuddyPersona = null;
    value.activeSessionId = null;
    value.sessions = [];
  }
  return value;
}

function withBuddy(revision, buddyId, overrides = {}) {
  return snapshot(revision, {
    activeBuddyId: buddyId,
    buddiesRevision: overrides.buddiesRevision ?? revision,
    ...overrides,
  });
}

function modelSettings(overrides = {}) {
  return {
    endpoint: 'http://127.0.0.1:4101/v1/chat/completions',
    model: 'model-a',
    persona: '苔 A',
    allowTransmission: false,
    hasApiKey: false,
    ...overrides,
  };
}

function committedModelSnapshot(
  revision,
  fromRevision,
  managementVersion,
  overrides = {},
) {
  return snapshot(revision, {
    buddiesRevision: fromRevision + 1,
    buddyCommit: {
      kind: 'configure_model',
      fromBuddyId: 'b-a',
      fromRevision,
      toBuddyId: 'b-a',
      toRevision: fromRevision + 1,
      managementVersion,
    },
    managementDraft: { version: managementVersion, dirty: true, kind: 'general' },
    modelSettings: modelSettings({
      endpoint: 'http://127.0.0.1:9/v1/chat/completions',
      model: 'model-applied',
      persona: '苔 A 已应用',
      hasApiKey: true,
    }),
    ...overrides,
  });
}

test('创建伙伴：只提交名称人设与精确草稿版本，上下文在发起时捕获；新伙伴不继承草稿或 API Key', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '新伙伴 C';
    instance.api.buddyForm.persona = '新伙伴的合成人设';
    await tick();
    await tick();
    const protectedCall = instance.managementCalls[instance.managementCalls.length - 1];
    assert.equal(protectedCall.dirty, true);
    assert.equal(protectedCall.kind, 'buddy_create');
    assert.deepEqual(protectedCall.context, {
      expectedBuddyId: 'b-a',
      expectedBuddiesRevision: 4,
    });
    const submittedVersion = protectedCall.version;
    instance.api.submitBuddyCreate();
    await tick();
    const call = instance.lastCall();
    assert.deepEqual(call.command, {
      kind: 'create_buddy',
      label: '新伙伴 C',
      persona: '新伙伴的合成人设',
      expectedManagementDraftVersion: submittedVersion,
    });
    assert.deepEqual(call.context, { expectedBuddyId: 'b-a', expectedBuddiesRevision: 4 });

    const created = snapshot(2, {
      activeBuddyId: 'b-c',
      buddiesRevision: 5,
      activeBuddyPersona: '新伙伴的合成人设',
      buddies: [...BUDDIES, buddyView('b-c', '新伙伴 C')],
      activeSessionId: 's-new',
      sessions: [session('s-new', '新会话')],
      chatDraft: { revision: 1, discard: null, sessions: [] },
      modelSettings: null,
      managementDraft: { version: submittedVersion, dirty: false, kind: 'none' },
      buddyCommit: {
        kind: 'create_buddy',
        fromBuddyId: 'b-a',
        fromRevision: 4,
        toBuddyId: 'b-c',
        toRevision: 5,
        managementVersion: submittedVersion,
      },
    });
    await settle(instance, { snapshot: created, error: null });
    assert.equal(instance.api.activeBuddyId.value, 'b-c');
    assert.equal(instance.api.buddies.value.length, 3);
    assert.equal(instance.api.buddyCreateOpen.value, false);
    assert.equal(instance.api.buddyForm.label, '');
    assert.equal(instance.api.modelForm.apiKeyInput, '');
    assert.equal(instance.api.modelForm.endpoint, '');
    assert.equal(instance.api.modelForm.persona, '新伙伴的合成人设');
    assert.match(instance.api.buddyNotice.value, /不继承/);
  } finally {
    instance.unmount();
  }
});

test('退出应用：同步未提交正文后携带原伙伴上下文请求整应用退出，不自动丢稿', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.onDraftInput({ target: { value: '退出前未提交稿' } });
    const quitting = instance.api.requestQuit();
    await tick();
    assert.equal(instance.windowCalls.length, 0);
    assert.equal(instance.lastCall().command.kind, 'set_draft');
    instance.lastCall().resolve({ snapshot: snapshot(2, {
      chat: { draft: '退出前未提交稿', messages: [], status: 'ready', failure: null },
    }), error: null });
    await quitting;
    assert.equal(instance.windowCalls.length, 1);
    assert.equal(instance.windowCalls[0].command, 'quit');
    assert.equal(instance.windowCalls[0].expectation.expectedBuddyId, 'b-a');
    assert.equal(instance.windowCalls[0].expectation.expectedBuddiesRevision, 4);
    assert.equal(instance.api.draft.value, '退出前未提交稿');
    assert.ok(instance.calls.every(call => call.command.kind !== 'confirm_close'));
  } finally { instance.unmount(); }
});

test('退出应用：管理同步期间切换伙伴会取消旧退出意图', async () => {
  const instance = component({ deferManagement: true });
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '受保护的新伙伴草稿';
    await tick();
    const quitting = instance.api.requestQuit();
    await tick();
    assert.equal(instance.windowCalls.length, 0);
    await load(instance, snapshot(2, { activeBuddyId: 'b-b', buddiesRevision: 5 }));
    for (const call of instance.managementCalls) call.resolve({ version: call.version, dirty: call.dirty });
    await quitting;
    assert.equal(instance.windowCalls.length, 0);
    assert.ok(instance.api.lastError.value);
  } finally { instance.unmount(); }
});

test('退出应用：撤权后仍可退出，不向已撤权草稿队列派发', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    await load(instance, snapshot(2, { revoked: true, activeBuddyId: null, buddies: [],
      activeSessionId: null, sessions: [] }));
    await instance.api.requestQuit();
    assert.equal(instance.calls.length, 0);
    assert.equal(instance.windowCalls.length, 1);
    assert.equal(instance.windowCalls[0].command, 'quit');
    assert.equal(instance.windowCalls[0].expectation.expectedBuddyId, '');
    assert.equal(instance.windowCalls[0].expectation.expectedBuddiesRevision, 4);
  } finally { instance.unmount(); }
});

test('退出应用：回复丢失不自动重发、不清正文，明确报告未确认', async () => {
  const instance = component({ rejectWindow: true });
  try {
    await load(instance, snapshot(1));
    await instance.api.requestQuit();
    await tick();
    assert.equal(instance.windowCalls.length, 1);
    assert.match(instance.api.lastError.value, /未确认/);
    assert.equal(instance.api.interactionLocked.value, false);
  } finally { instance.unmount(); }
});

test('切换伙伴：同局部会话 ID 的草稿按伙伴隔离，A→B→A 各自恢复且不串档', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    // 伙伴 A 的草稿同步到宿主。
    instance.api.onDraftInput({ target: { value: 'A 的草稿' }, isComposing: false });
    await tick();
    const draftCallA = instance.lastCall();
    assert.deepEqual(draftCallA.command, {
      kind: 'set_draft',
      text: 'A 的草稿',
      expectedSessionId: 's-shared',
      expectedSessionsRevision: 3,
    });
    assert.deepEqual(draftCallA.context, { expectedBuddyId: 'b-a', expectedBuddiesRevision: 4 });
    await settle(instance, {
      snapshot: snapshot(2, { chat: { draft: 'A 的草稿' } }),
      error: null,
    });
    assert.equal(instance.api.draft.value, 'A 的草稿');

    // 切到伙伴 B：命令上下文是发起时的 A，回包同局部会话 ID 也不能带回 A 的正文。
    instance.api.switchToBuddy('b-b');
    await tick();
    const switchCall = instance.lastCall();
    assert.deepEqual(switchCall.command, { kind: 'switch_buddy', id: 'b-b' });
    assert.deepEqual(switchCall.context, { expectedBuddyId: 'b-a', expectedBuddiesRevision: 4 });
    await settle(instance, {
      snapshot: withBuddy(3, 'b-b', { buddiesRevision: 5, chat: { draft: '' } }),
      error: null,
    });
    assert.equal(instance.api.activeBuddyId.value, 'b-b');
    assert.equal(instance.api.draft.value, '', 'B 的同名局部会话不继承 A 的草稿');
    assert.equal(instance.api.buddyDrafts.peek('b-a', 's-shared').text, 'A 的草稿');

    // 伙伴 B 编辑自己的草稿。
    instance.api.onDraftInput({ target: { value: 'B 的草稿' }, isComposing: false });
    await tick();
    assert.equal(instance.lastCall().context.expectedBuddyId, 'b-b');
    await settle(instance, {
      snapshot: withBuddy(4, 'b-b', { buddiesRevision: 5, chat: { draft: 'B 的草稿' } }),
      error: null,
    });

    // A→B→A：切回 A 恢复 A 自己的草稿，B 的草稿仍在 B 槽位。
    instance.api.switchToBuddy('b-a');
    await tick();
    assert.deepEqual(instance.lastCall().context, {
      expectedBuddyId: 'b-b',
      expectedBuddiesRevision: 5,
    });
    await settle(instance, {
      snapshot: withBuddy(5, 'b-a', { buddiesRevision: 6, chat: { draft: 'A 的草稿' } }),
      error: null,
    });
    assert.equal(instance.api.activeBuddyId.value, 'b-a');
    assert.equal(instance.api.draft.value, 'A 的草稿');
    assert.equal(instance.api.buddyDrafts.peek('b-a', 's-shared').text, 'A 的草稿');
    assert.equal(instance.api.buddyDrafts.peek('b-b', 's-shared').text, 'B 的草稿');

    // 选择修订向前推进，A→B→A 不允许回到旧修订。
    instance.api.switchToBuddy('b-b');
    await tick();
    assert.deepEqual(instance.lastCall().context, {
      expectedBuddyId: 'b-a',
      expectedBuddiesRevision: 6,
    });
  } finally {
    instance.unmount();
  }
});

test('隐藏聊天在发起时捕获完整伙伴上下文，排队期间换伙伴不改写本次命令', async () => {
  const instance = component({ deferWindow: true });
  try {
    await load(instance, snapshot(1));
    const hiding = instance.api.hideChat();
    await tick();
    assert.equal(instance.windowCalls.length, 1);
    assert.equal(instance.windowCalls[0].command, 'hide_chat');
    assert.deepEqual(instance.windowCalls[0].expectation, {
      expectedBuddyId: 'b-a',
      expectedBuddiesRevision: 4,
    });

    // 宿主在窗口命令排队期间推进到另一个伙伴；本次命令仍保留发起上下文。
    await load(instance, withBuddy(2, 'b-b', { buddiesRevision: 5 }));
    assert.equal(instance.api.activeBuddyId.value, 'b-b');
    assert.deepEqual(instance.windowCalls[0].expectation, {
      expectedBuddyId: 'b-a',
      expectedBuddiesRevision: 4,
    });
    instance.windowCalls[0].resolve();
    await hiding;
    await tick();

    // 下一次用户发起才使用新的完整上下文。
    const second = instance.api.hideChat();
    await tick();
    assert.equal(instance.windowCalls.length, 2);
    assert.equal(instance.windowCalls[1].command, 'hide_chat');
    assert.deepEqual(instance.windowCalls[1].expectation, {
      expectedBuddyId: 'b-b',
      expectedBuddiesRevision: 5,
    });
    instance.windowCalls[1].resolve();
    await second;
  } finally {
    instance.unmount();
  }
});

test('改名只推进档案版本：身份稳定、人设/模型版本不变，未推进则如实报告未确认', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyRename();
    assert.equal(instance.api.buddyRenameLabel.value, '伙伴 A');
    instance.api.buddyRenameLabel.value = '伙伴 A 新名';
    await tick();
    await tick();
    const protectedCall = instance.managementCalls[instance.managementCalls.length - 1];
    assert.equal(protectedCall.dirty, true);
    assert.equal(protectedCall.kind, 'buddy_rename');
    instance.api.submitBuddyRename();
    await tick();
    const call = instance.lastCall();
    assert.deepEqual(call.command, {
      kind: 'rename_buddy',
      label: '伙伴 A 新名',
      expectedManagementDraftVersion: protectedCall.version,
    });
    assert.deepEqual(call.context, { expectedBuddyId: 'b-a', expectedBuddiesRevision: 4 });

    const renamed = snapshot(2, {
      buddiesRevision: 5,
      buddies: [buddyView('b-a', '伙伴 A 新名', { profileRevision: 2 }), BUDDIES[1]],
      managementDraft: { version: protectedCall.version, dirty: false, kind: 'none' },
      buddyCommit: {
        kind: 'rename_buddy',
        fromBuddyId: 'b-a',
        fromRevision: 4,
        toBuddyId: 'b-a',
        toRevision: 5,
        managementVersion: protectedCall.version,
      },
    });
    await settle(instance, { snapshot: renamed, error: null });
    assert.equal(instance.api.buddyRenameOpen.value, false);
    assert.equal(instance.api.activeBuddyId.value, 'b-a', '改名不改变实例身份');
    const active = instance.api.buddies.value.find((buddy) => buddy.id === 'b-a');
    assert.equal(active.label, '伙伴 A 新名');
    assert.equal(active.profileRevision, 2);
    assert.equal(active.personaRevision, 1);
    assert.equal(active.modelRevision, 1);

    // 宿主未推进档案版本：不伪造成功，保持原选择与表单。
    instance.api.openBuddyRename();
    instance.api.buddyRenameLabel.value = '另一次改名';
    await tick();
    await tick();
    const secondProtected = instance.managementCalls[instance.managementCalls.length - 1];
    instance.api.submitBuddyRename();
    await tick();
    await settle(instance, {
      snapshot: snapshot(3, {
        buddiesRevision: 6,
        buddies: [buddyView('b-a', '另一次改名'), BUDDIES[1]],
        managementDraft: { version: secondProtected.version, dirty: false, kind: 'none' },
      }),
      error: null,
    });
    assert.match(instance.api.lastError.value, /未确认本次改名/);
    assert.equal(
      instance.api.buddies.value.find((buddy) => buddy.id === 'b-a').label,
      '另一次改名',
      '显示以宿主权威快照为准，但明确报告未确认',
    );
  } finally {
    instance.unmount();
  }
});

test('未保存管理编辑阻止伙伴切换：界面不派发，也不清管理草稿', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.activePanel.value = 'presets';
    instance.api.modelForm.persona = '未保存的人设修改';
    await tick();
    await tick();
    assert.notEqual(instance.api.buddyActionBlockReason.value, null);
    assert.match(instance.api.buddyActionBlockReason.value, /未保存编辑/);
    const dispatchCount = instance.calls.length;
    instance.api.switchToBuddy('b-b');
    await tick();
    assert.equal(instance.calls.length, dispatchCount, '阻断时不得派发 switch_buddy');
    assert.match(instance.api.buddyError.value, /未保存编辑/);
    assert.equal(instance.api.modelForm.persona, '未保存的人设修改');
  } finally {
    instance.unmount();
  }
});

test('延迟模型回包不把旧伙伴的配置或 API Key 写回新伙伴', async () => {
  const instance = component();
  try {
    const settingsA = {
      endpoint: 'http://127.0.0.1:4101/v1/chat/completions',
      model: 'model-a',
      persona: '苔 A',
      allowTransmission: false,
      hasApiKey: false,
    };
    await load(instance, snapshot(1, { modelSettings: settingsA }));
    instance.api.activePanel.value = 'presets';
    instance.api.modelForm.apiKeyInput = 'secret-A';
    const applying = instance.api.applyModelSettings();
    await tick();
    const call = instance.lastCall();
    assert.equal(call.command.kind, 'configure_model');
    assert.equal(call.command.apiKey, 'secret-A');
    assert.deepEqual(call.context, { expectedBuddyId: 'b-a', expectedBuddiesRevision: 4 });

    // 请求在途时宿主切到伙伴 B；B 没有模型配置。
    await load(instance, withBuddy(2, 'b-b', { buddiesRevision: 5 }));
    assert.equal(instance.api.modelForm.apiKeyInput, '', '密码框状态不得跨伙伴保留');
    assert.equal(instance.api.modelForm.endpoint, '');
    assert.equal(instance.api.activePanel.value, null, '切换伙伴关闭旧伙伴管理面板');

    // 迟到的 A 回包（较旧修订）不能回填，也不能复活 A 的配置。
    call.resolve({
      snapshot: snapshot(1, {
        modelSettings: { ...settingsA, hasApiKey: true, persona: '苔 A 已应用' },
      }),
      error: null,
    });
    await applying;
    await tick();
    assert.equal(instance.api.modelForm.apiKeyInput, '');
    assert.equal(instance.api.modelForm.endpoint, '');
    assert.equal(instance.api.activeBuddyId.value, 'b-b');
  } finally {
    instance.unmount();
  }
});

test('撤权清空全部伙伴草稿与水位；未知锁迁移后仍保守锁定', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.onDraftInput({ target: { value: 'A 的草稿' }, isComposing: false });
    await tick();
    await settle(instance, { snapshot: snapshot(2, { chat: { draft: 'A 的草稿' } }), error: null });
    instance.api.switchToBuddy('b-b');
    await tick();
    await settle(instance, {
      snapshot: withBuddy(3, 'b-b', { buddiesRevision: 5, chat: { draft: '' } }),
      error: null,
    });
    instance.api.onDraftInput({ target: { value: 'B 的草稿' }, isComposing: false });
    await tick();
    await settle(instance, {
      snapshot: withBuddy(4, 'b-b', { buddiesRevision: 5, chat: { draft: 'B 的草稿' } }),
      error: null,
    });
    assert.equal(instance.api.buddyDrafts.scopeCount, 2);

    await load(instance, snapshot(5, { revoked: true }));
    assert.equal(instance.api.draft.value, '');
    assert.equal(instance.api.buddyDrafts.scopeCount, 0);
    assert.equal(instance.api.activeBuddyId.value, null);
    assert.equal(instance.api.activeBuddy.value, null);
  } finally {
    instance.unmount();
  }
});

test('无伙伴注册表时创建/切换不可用，命令上下文为显式空哨兵', async () => {
  const instance = component();
  try {
    await load(
      instance,
      snapshot(1, {
        activeBuddyId: null,
        buddies: [],
        buddiesRevision: 2,
        activeBuddyPersona: null,
      }),
    );
    assert.equal(instance.api.activeBuddy.value, null);
    assert.equal(instance.api.buddies.value.length, 0);
    assert.deepEqual(instance.api.buddyTarget, {
      expectedBuddyId: '',
      expectedBuddiesRevision: 2,
    });
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = 'NaN';
    instance.api.buddyForm.persona = '空哨兵下的合成人设';
    await tick();
    await tick();
    const protectedCall = instance.managementCalls[instance.managementCalls.length - 1];
    assert.equal(protectedCall.kind, 'buddy_create');
    instance.api.submitBuddyCreate();
    await tick();
    assert.equal(instance.api.buddyError.value, null);
    const call = instance.lastCall();
    assert.deepEqual(call.context, { expectedBuddyId: '', expectedBuddiesRevision: 2 });
    assert.deepEqual(call.command, {
      kind: 'create_buddy',
      label: 'NaN',
      persona: '空哨兵下的合成人设',
      expectedManagementDraftVersion: protectedCall.version,
    });

    // 未选择伙伴时切换一个不在注册表的 ID 必须本地拒绝，不能凭空构造身份。
    instance.calls.length = 0;
    await instance.api.switchToBuddy('b-missing');
    assert.equal(instance.calls.length, 0);
    assert.match(instance.api.buddyError.value, /不在宿主签发的注册表/);
    // 空串哨兵不是可用伙伴 ID，不能成为业务目标或发送上下文。
    await instance.api.switchToBuddy('');
    assert.equal(instance.calls.length, 0);
    assert.equal(instance.api.canSend.value, false);
    assert.match(instance.api.sendBlockade.value, /空哨兵|未签发/);
  } finally {
    instance.unmount();
  }
});

test('改名/创建本地校验不静默裁剪：空名、超长名与控制字符拒绝派发', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '   ';
    instance.api.submitBuddyCreate();
    await tick();
    assert.match(instance.api.buddyError.value, /不能为空/);
    assert.equal(instance.calls.length, 0);

    instance.api.buddyForm.label = 'a'.repeat(65);
    instance.api.submitBuddyCreate();
    await tick();
    assert.match(instance.api.buddyError.value, /不能超过 64/);
    assert.equal(instance.calls.length, 0);

    instance.api.buddyForm.label = '合法名称';
    instance.api.buddyForm.persona = 'x'.repeat(4097);
    instance.api.submitBuddyCreate();
    await tick();
    assert.match(instance.api.buddyError.value, /人设不能超过/);
    assert.equal(instance.calls.length, 0);
  } finally {
    instance.unmount();
  }
});

test('管理编辑同步在发起时携带当前伙伴上下文', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.activePanel.value = 'presets';
    instance.api.modelForm.persona = '需要退出保护的人设草稿';
    await tick();
    await tick();
    assert.ok(instance.managementCalls.length >= 1, 'no management draft state was dispatched');
    const last = instance.managementCalls[instance.managementCalls.length - 1];
    assert.equal(last.dirty, true);
    assert.equal(last.kind, 'general');
    assert.deepEqual(last.context, { expectedBuddyId: 'b-a', expectedBuddiesRevision: 4 });
  } finally {
    instance.unmount();
  }
});

test('伙伴选择回包丢失产生的未知在撤权后仍保守锁定', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.switchToBuddy('b-b');
    await tick();
    const call = instance.lastCall();
    call.reject(new Error('synthetic ipc loss'));
    await tick();
    await tick();
    assert.equal(instance.api.commandLock.locked, true);
    assert.equal(instance.api.commandLock.operation, 'switch_buddy');
    assert.equal(instance.api.commandLock.currentBuddy, 'b-a');

    await load(instance, snapshot(2, { revoked: true }));
    assert.equal(instance.api.commandLock.locked, true, '撤权不得把未知当成已收口');
    assert.equal(instance.api.commandLock.currentBuddy, '');
  } finally {
    instance.unmount();
  }
});

test('未提交的创建/改名表单接入管理草稿同步：种类与版本随编辑推进', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    // 领队探针的正式化：直接写响应式表单也要触发 dirty 通知。
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '未提交伙伴';
    instance.api.buddyForm.persona = '未提交人设';
    await tick();
    await tick();
    const createCalls = instance.managementCalls.filter(
      (call) => call.kind === 'buddy_create' && call.dirty === true,
    );
    assert.ok(createCalls.length >= 1, 'unsaved buddy creation must notify trusted exit protection');
    assert.deepEqual(createCalls[createCalls.length - 1].context, {
      expectedBuddyId: 'b-a',
      expectedBuddiesRevision: 4,
    });

    instance.api.cancelBuddyCreate();
    await tick();
    await tick();
    assert.equal(instance.api.buddyCreateOpen.value, false);
    const cleaned = instance.managementCalls[instance.managementCalls.length - 1];
    assert.equal(cleaned.dirty, false);
    assert.equal(cleaned.kind, 'none');

    instance.api.openBuddyRename();
    instance.api.buddyRenameLabel.value = '未提交新名';
    await tick();
    await tick();
    const renameCalls = instance.managementCalls.filter(
      (call) => call.kind === 'buddy_rename' && call.dirty === true,
    );
    assert.ok(renameCalls.length >= 1, 'unsaved buddy rename must notify trusted exit protection');
  } finally {
    instance.unmount();
  }
});

test('未保存创建表单阻止切换与打开改名，且不静默丢稿', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '受保护的创建稿';
    instance.api.buddyForm.persona = '受保护的创建人设';
    await tick();
    await tick();
    const dispatched = instance.calls.length;
    await instance.api.switchToBuddy('b-b');
    assert.equal(instance.calls.length, dispatched, 'dirty buddy form must block switching');
    assert.equal(instance.api.buddyCreateOpen.value, true);
    assert.equal(instance.api.buddyForm.label, '受保护的创建稿');
    assert.match(instance.api.buddyError.value, /未保存编辑/);

    instance.api.openBuddyRename();
    assert.equal(instance.api.buddyRenameOpen.value, false);
    assert.equal(instance.api.buddyCreateOpen.value, true);
    assert.equal(instance.api.buddyForm.persona, '受保护的创建人设');
  } finally {
    instance.unmount();
  }
});

test('创建保存失败不撤保护、不清稿，且提交前不先发 clean 状态', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '失败保留伙伴';
    instance.api.buddyForm.persona = '失败保留人设';
    await tick();
    await tick();
    const submittedVersion = instance.managementCalls[instance.managementCalls.length - 1].version;
    const beforeSubmit = instance.managementCalls.length;
    instance.api.submitBuddyCreate();
    await tick();
    const call = instance.lastCall();
    assert.equal(call.command.kind, 'create_buddy');
    assert.equal(call.command.expectedManagementDraftVersion, submittedVersion);
    // 提交期间仍保持 dirty；没有先撤掉退出保护再发请求。
    const during = instance.managementCalls.slice(beforeSubmit);
    assert.ok(
      during.every((item) => item.dirty === true && item.kind === 'buddy_create'),
      JSON.stringify(during),
    );

    await settle(instance, {
      snapshot: snapshot(2, {
        managementDraft: { version: submittedVersion, dirty: true, kind: 'buddy_create' },
      }),
      error: {
        code: 'invalid_buddy',
        message: 'synthetic rejection',
        disposition: 'not_executed',
      },
    });
    assert.equal(instance.api.buddyCreateOpen.value, true);
    assert.equal(instance.api.buddyForm.label, '失败保留伙伴');
    assert.equal(instance.api.buddyForm.persona, '失败保留人设');
    assert.match(instance.api.lastError.value, /未执行|synthetic/);
    const last = instance.managementCalls[instance.managementCalls.length - 1];
    assert.equal(last.dirty, true);
    assert.equal(last.kind, 'buddy_create');
  } finally {
    instance.unmount();
  }
});

test('提交期间的新编辑不被迟到成功清掉', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyRename();
    instance.api.buddyRenameLabel.value = '第一次改名';
    await tick();
    await tick();
    const submittedVersion = instance.managementCalls[instance.managementCalls.length - 1].version;
    instance.api.submitBuddyRename();
    await tick();
    assert.equal(instance.lastCall().command.expectedManagementDraftVersion, submittedVersion);

    // 回包前继续编辑：版本推进到新值，成功只能确认旧版本。
    instance.api.buddyRenameLabel.value = '第二次改名';
    await tick();
    await tick();
    await settle(instance, {
      snapshot: snapshot(2, {
        buddiesRevision: 5,
        buddies: [buddyView('b-a', '第一次改名', { profileRevision: 2 }), BUDDIES[1]],
        managementDraft: { version: submittedVersion, dirty: false, kind: 'none' },
        buddyCommit: {
          kind: 'rename_buddy',
          fromBuddyId: 'b-a',
          fromRevision: 4,
          toBuddyId: 'b-a',
          toRevision: 5,
          managementVersion: submittedVersion,
        },
      }),
      error: null,
    });
    assert.equal(instance.api.buddyRenameOpen.value, true, 'newer edit must keep the form');
    assert.equal(instance.api.buddyRenameLabel.value, '第二次改名');
    assert.match(instance.api.lastError.value, /新编辑/);
    const last = instance.managementCalls[instance.managementCalls.length - 1];
    assert.equal(last.dirty, true);
    assert.equal(last.kind, 'buddy_rename');
  } finally {
    instance.unmount();
  }
});

test('同伙伴选择修订推进：排队的管理编辑不换新修订，表单正文保留并在新作用域重新记录', async () => {
  const instance = component({ deferManagement: true });
  try {
    await load(instance, snapshot(1));
    for (const call of instance.managementCalls) {
      call.resolve({ version: call.version, dirty: call.dirty });
    }
    await tick();
    instance.managementCalls.length = 0;
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '第一份创建稿';
    instance.api.buddyForm.persona = '受保护人设';
    await tick();
    instance.api.buddyForm.label = '第二份创建稿';
    await tick();
    assert.equal(instance.managementCalls.length, 1, '第一份在途，第二份只排队');
    const first = instance.managementCalls[0];
    assert.deepEqual(first.context, { expectedBuddyId: 'b-a', expectedBuddiesRevision: 4 });

    // 宿主在同一伙伴上推进选择修订：旧队列不得把待发编辑换成修订 5。
    await load(instance, snapshot(2, { buddiesRevision: 5 }));
    assert.equal(instance.api.activeBuddyId.value, 'b-a');
    assert.equal(instance.managementCalls.length, 1, '修订推进本身不自动补发旧编辑');
    first.resolve({ version: first.version, dirty: first.dirty });
    await tick();
    await tick();
    assert.equal(instance.managementCalls.length, 1, '旧排队编辑不得使用新修订发出');
    assert.equal(
      instance.managementCalls.some((call) => call.context.expectedBuddiesRevision === 5),
      false,
    );

    // 正文没有丢；只有用户在新作用域继续编辑才建立新同步。
    assert.equal(instance.api.buddyCreateOpen.value, true);
    assert.equal(instance.api.buddyForm.label, '第二份创建稿');
    assert.equal(instance.api.buddyForm.persona, '受保护人设');
    instance.api.buddyForm.label = '第三份创建稿';
    await tick();
    await tick();
    const last = instance.managementCalls[instance.managementCalls.length - 1];
    assert.deepEqual(last.context, { expectedBuddyId: 'b-a', expectedBuddiesRevision: 5 });
    assert.equal(last.dirty, true);
    assert.equal(last.kind, 'buddy_create');
  } finally {
    instance.unmount();
    for (const call of instance.managementCalls) {
      if (typeof call.resolve === 'function') {
        call.resolve({ version: call.version, dirty: call.dirty });
      }
    }
  }
});

test('同伙伴选择修订推进后，旧修订的模型回包不回填表单或清 API Key', async () => {
  const instance = component();
  try {
    const settings = {
      endpoint: 'http://127.0.0.1:4101/v1/chat/completions',
      model: 'model-a',
      persona: '苔 A',
      allowTransmission: false,
      hasApiKey: false,
    };
    await load(instance, snapshot(1, { modelSettings: settings }));
    instance.api.activePanel.value = 'presets';
    instance.api.modelForm.apiKeyInput = 'secret-A';
    const applying = instance.api.applyModelSettings();
    await tick();
    const call = instance.lastCall();
    assert.equal(call.command.kind, 'configure_model');
    assert.deepEqual(call.context, { expectedBuddyId: 'b-a', expectedBuddiesRevision: 4 });

    // 同一伙伴选择修订推进；旧修订的模型回包不得回填端点/人设，也不清密码框。
    await load(instance, snapshot(2, { buddiesRevision: 5 }));
    assert.equal(instance.api.activeBuddyId.value, 'b-a');
    assert.equal(instance.api.activePanel.value, 'presets');
    call.resolve({
      snapshot: snapshot(3, {
        buddiesRevision: 5,
        modelSettings: {
          ...settings,
          endpoint: 'http://127.0.0.1:9/v1/chat/completions',
          persona: '苔 A 旧修订回填',
          hasApiKey: true,
        },
      }),
      error: null,
    });
    await applying;
    await tick();
    assert.equal(instance.api.modelForm.endpoint, settings.endpoint);
    assert.equal(instance.api.modelForm.persona, '苔 A');
    assert.equal(instance.api.modelForm.apiKeyInput, 'secret-A');
  } finally {
    instance.unmount();
  }
});

test('同伙伴选择修订推进后，旧修订的记忆回包不清新草稿或写回提示', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.activePanel.value = 'memory';
    instance.api.rememberDraft.value = '旧修订待记住正文';
    const remembering = instance.api.rememberMemory();
    await tick();
    const call = instance.lastCall();
    assert.equal(call.command.kind, 'remember');
    assert.deepEqual(call.context, { expectedBuddyId: 'b-a', expectedBuddiesRevision: 4 });
    // 旧修订请求在途时继续编辑，随后宿主推进选择修订。
    instance.api.rememberDraft.value = '新修订继续编辑的正文';
    await load(instance, snapshot(2, { buddiesRevision: 5 }));
    assert.equal(instance.api.activePanel.value, 'memory');
    call.resolve({
      snapshot: snapshot(3, {
        buddiesRevision: 5,
        memories: [],
      }),
      error: null,
    });
    await remembering;
    await tick();
    assert.equal(instance.api.rememberDraft.value, '新修订继续编辑的正文');
    assert.equal(instance.api.memoryNotice.value, null, '旧修订回包不得写回成功提示');
  } finally {
    instance.unmount();
  }
});

test('A→B→A 后迟到的改名成功不清当前表单与草稿', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.onDraftInput({ target: { value: 'A 的草稿' }, isComposing: false });
    await tick();
    await settle(instance, {
      snapshot: snapshot(2, { chat: { draft: 'A 的草稿' } }),
      error: null,
    });
    assert.equal(instance.api.draft.value, 'A 的草稿');

    instance.api.openBuddyRename();
    instance.api.buddyRenameLabel.value = 'A 的迟到改名';
    await tick();
    await tick();
    const submittedVersion = instance.managementCalls[instance.managementCalls.length - 1].version;
    instance.api.submitBuddyRename();
    await tick();
    const lateCall = instance.lastCall();
    assert.equal(lateCall.command.kind, 'rename_buddy');

    // 宿主选择 A→B→A；B 有自己的人设，A 自己的草稿按伙伴槽位恢复。
    await load(
      instance,
      withBuddy(3, 'b-b', {
        buddiesRevision: 5,
        activeBuddyPersona: '伙伴 B 的人设',
        managementDraft: { version: submittedVersion + 1, dirty: false, kind: 'none' },
      }),
    );
    assert.equal(instance.api.activeBuddyId.value, 'b-b');
    await load(
      instance,
      withBuddy(4, 'b-a', {
        buddiesRevision: 6,
        activeBuddyPersona: '伙伴 A 的合成人设',
        chat: { draft: 'A 的草稿' },
        managementDraft: { version: submittedVersion + 1, dirty: false, kind: 'none' },
      }),
    );
    assert.equal(instance.api.activeBuddyId.value, 'b-a');
    assert.equal(instance.api.draft.value, 'A 的草稿');

    // 迟到的 A 成功回包是较旧展示修订，不能清掉当前表单或草稿。
    lateCall.resolve({
      snapshot: snapshot(2, {
        activeBuddyId: 'b-a',
        buddiesRevision: 7,
        buddies: [buddyView('b-a', 'A 的迟到改名', { profileRevision: 2 }), BUDDIES[1]],
        managementDraft: { version: submittedVersion, dirty: false, kind: 'none' },
      }),
      error: null,
    });
    await tick();
    await tick();
    assert.equal(instance.api.activeBuddyId.value, 'b-a');
    assert.equal(instance.api.buddyRenameOpen.value, false);
    assert.equal(instance.api.draft.value, 'A 的草稿');
    assert.match(instance.api.lastError.value ?? '', /档案版本|作用域|新编辑/);
  } finally {
    instance.unmount();
  }
});

test('明确丢弃只清匹配版本的表单，旧回执不清新编辑', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '待丢弃草稿';
    instance.api.buddyForm.persona = '待丢弃人设';
    await tick();
    await tick();
    const discardedVersion = instance.managementCalls[instance.managementCalls.length - 1].version;

    await load(
      instance,
      snapshot(2, {
        activeBuddyPersona: '伙伴 A 的合成人设',
        managementDraft: { version: discardedVersion, dirty: false, kind: 'none' },
        managementDiscardedVersion: discardedVersion,
      }),
    );
    assert.equal(instance.api.buddyCreateOpen.value, false);
    assert.equal(instance.api.buddyForm.label, '');
    assert.equal(instance.api.buddyForm.persona, '');

    // 新编辑的版本高于旧丢弃回执，旧回执不得清稿。
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '新的创建稿';
    instance.api.buddyForm.persona = '新的创建人设';
    await tick();
    await tick();
    const newerVersion = instance.managementCalls[instance.managementCalls.length - 1].version;
    assert.ok(newerVersion > discardedVersion);
    await load(
      instance,
      snapshot(3, {
        activeBuddyPersona: '伙伴 A 的合成人设',
        managementDraft: { version: newerVersion, dirty: true, kind: 'buddy_create' },
        managementDiscardedVersion: discardedVersion,
      }),
    );
    assert.equal(instance.api.buddyCreateOpen.value, true);
    assert.equal(instance.api.buddyForm.label, '新的创建稿');
    assert.equal(instance.api.buddyForm.persona, '新的创建人设');
  } finally {
    instance.unmount();
  }
});

test('activeBuddyPersona 初始化未配置模型表单，空表单不覆盖已绑定人设', async () => {
  const instance = component();
  try {
    await load(
      instance,
      snapshot(1, {
        activeBuddyPersona: '伙伴 A 的绑定人设',
        modelSettings: null,
      }),
    );
    assert.equal(instance.api.modelForm.persona, '伙伴 A 的绑定人设', '首个快照即初始化表单');
    instance.api.activePanel.value = 'presets';
    instance.api.syncModelForm(true);
    assert.equal(instance.api.modelForm.persona, '伙伴 A 的绑定人设');
    assert.equal(instance.api.modelForm.endpoint, '');

    await load(
      instance,
      withBuddy(2, 'b-b', {
        buddiesRevision: 5,
        activeBuddyPersona: '伙伴 B 的人设',
        modelSettings: null,
      }),
    );
    assert.equal(instance.api.modelForm.persona, '伙伴 B 的人设', '不回填旧伙伴人设');
    assert.equal(instance.api.modelForm.endpoint, '', '不回填旧伙伴端点');

    // 空白人设不能被当成一次有效保存，也不清空已绑定人设。
    instance.api.modelForm.persona = '';
    instance.api.modelForm.endpoint = 'http://127.0.0.1:9/v1/chat/completions';
    instance.api.modelForm.model = 'synthetic-model';
    const dispatched = instance.calls.length;
    await instance.api.applyModelSettings();
    assert.equal(instance.calls.length, dispatched, 'empty persona must not be dispatched');
    assert.match(instance.api.modelApplyNotice.value ?? '', /人设不能为空/);
    instance.api.syncModelForm(true);
    assert.equal(instance.api.modelForm.persona, '伙伴 B 的人设');
  } finally {
    instance.unmount();
  }
});

test('匹配 buddyCommit 的事件先到：模型保存成功清密码与 dirty，迟到回复不重复消费', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1, { modelSettings: modelSettings() }));
    instance.api.activePanel.value = 'presets';
    instance.api.modelForm.endpoint = 'http://127.0.0.1:9/v1/chat/completions';
    instance.api.modelForm.model = 'model-next';
    instance.api.modelForm.persona = '苔 A 新配置';
    instance.api.modelForm.apiKeyInput = 'secret-commit-a';
    await tick();
    const applying = instance.api.applyModelSettings();
    await tick();
    await tick();
    const call = instance.lastCall();
    assert.equal(call.command.kind, 'configure_model');
    const managementVersion = instance.api.managementSync.value.version;
    const committed = committedModelSnapshot(2, 4, managementVersion);

    // 快照事件先到：匹配回执立即结算本次保存。
    await load(instance, committed);
    assert.equal(instance.api.modelForm.apiKeyInput, '');
    assert.equal(instance.api.modelForm.persona, '苔 A 已应用');
    assert.equal(instance.api.modelPanelDirty.value, false);
    assert.match(instance.api.modelApplyNotice.value ?? '', /已应用/);
    assert.equal(instance.api.commandLock.locked, false);

    // 调用回复随后到：同一回执只消费一次，不覆盖当前表单。
    call.resolve({ snapshot: committed, error: null });
    await applying;
    await tick();
    assert.equal(instance.api.modelForm.apiKeyInput, '');
    assert.equal(instance.api.modelForm.persona, '苔 A 已应用');
  } finally {
    instance.unmount();
  }
});

test('匹配 buddyCommit 的回复先到：正常清 Key 与已提交 dirty', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1, { modelSettings: modelSettings() }));
    instance.api.activePanel.value = 'presets';
    instance.api.modelForm.endpoint = 'http://127.0.0.1:9/v1/chat/completions';
    instance.api.modelForm.persona = '苔 A 新配置';
    instance.api.modelForm.apiKeyInput = 'secret-commit-b';
    await tick();
    const applying = instance.api.applyModelSettings();
    await tick();
    await tick();
    const call = instance.lastCall();
    const managementVersion = instance.api.managementSync.value.version;
    call.resolve({ snapshot: committedModelSnapshot(2, 4, managementVersion), error: null });
    await applying;
    await tick();
    assert.equal(instance.api.modelForm.apiKeyInput, '');
    assert.equal(instance.api.modelForm.persona, '苔 A 已应用');
    assert.equal(instance.api.modelPanelDirty.value, false);
    assert.equal(instance.api.commandLock.locked, false);
    const lastManagement = instance.managementCalls[instance.managementCalls.length - 1];
    assert.equal(lastManagement.dirty, false);
    assert.equal(lastManagement.kind, 'none');
  } finally {
    instance.unmount();
  }
});

test('回复丢失后重读同一 buddyCommit：消费成功并收口本次 Unknown', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1, { modelSettings: modelSettings() }));
    instance.api.activePanel.value = 'presets';
    instance.api.modelForm.endpoint = 'http://127.0.0.1:9/v1/chat/completions';
    instance.api.modelForm.apiKeyInput = 'secret-commit-c';
    await tick();
    const applying = instance.api.applyModelSettings();
    await tick();
    await tick();
    const call = instance.lastCall();
    const managementVersion = instance.api.managementSync.value.version;
    call.reject(new Error('synthetic ipc reply lost'));
    await applying;
    await tick();
    assert.equal(instance.api.commandLock.locked, true, 'lost reply must lock as unknown');

    // 重读得到同一无正文回执：证明本次保存已完成，但不伪造新结果。
    await load(instance, committedModelSnapshot(2, 4, managementVersion));
    assert.equal(instance.api.modelForm.apiKeyInput, '');
    assert.equal(instance.api.modelPanelDirty.value, false);
    assert.equal(instance.api.commandLock.locked, false, 'matching receipt must settle its own unknown');
    assert.match(instance.api.modelApplyNotice.value ?? '', /已应用/);
  } finally {
    instance.unmount();
  }
});

test('回执到达前有新编辑：只确认旧保存，不清新编辑或 Key', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1, { modelSettings: modelSettings() }));
    instance.api.activePanel.value = 'presets';
    instance.api.modelForm.endpoint = 'http://127.0.0.1:9/v1/chat/completions';
    instance.api.modelForm.apiKeyInput = 'secret-commit-d';
    await tick();
    const applying = instance.api.applyModelSettings();
    await tick();
    await tick();
    const call = instance.lastCall();
    const managementVersion = instance.api.managementSync.value.version;
    instance.api.modelForm.persona = '回执前的新编辑人设';
    instance.api.noteModelInteraction();
    await tick();
    call.resolve({ snapshot: committedModelSnapshot(2, 4, managementVersion), error: null });
    await applying;
    await tick();
    assert.equal(instance.api.modelForm.apiKeyInput, 'secret-commit-d');
    assert.equal(instance.api.modelForm.persona, '回执前的新编辑人设');
    assert.equal(instance.api.modelPanelDirty.value, true);
    assert.equal(instance.api.commandLock.locked, false);
    assert.doesNotMatch(instance.api.modelApplyNotice.value ?? '', /已应用/);
  } finally {
    instance.unmount();
  }
});

test('无关迟到结果不消费；显式再次保存作为新 context 意图成功', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1, { modelSettings: modelSettings() }));
    instance.api.activePanel.value = 'presets';
    instance.api.modelForm.endpoint = 'http://127.0.0.1:9/v1/chat/completions';
    instance.api.modelForm.apiKeyInput = 'secret-commit-e';
    await tick();
    const first = instance.api.applyModelSettings();
    await tick();
    await tick();
    const firstCall = instance.lastCall();
    const firstVersion = instance.api.managementSync.value.version;
    // 同伙伴修订推进但没有匹配回执：不得把配置成功写回。
    await load(instance, snapshot(2, {
      buddiesRevision: 5,
      managementDraft: { version: firstVersion, dirty: true, kind: 'general' },
    }));
    firstCall.resolve({
      snapshot: snapshot(3, {
        buddiesRevision: 5,
        managementDraft: { version: firstVersion, dirty: true, kind: 'general' },
      }),
      error: null,
    });
    await first;
    await tick();
    assert.equal(instance.api.modelForm.apiKeyInput, 'secret-commit-e');
    assert.equal(instance.api.modelForm.persona, '苔 A');

    // 不要求用户再改字符：下次显式保存使用当前 r5 context 重新同步。
    const second = instance.api.applyModelSettings();
    await tick();
    await tick();
    const secondCall = instance.lastCall();
    assert.deepEqual(secondCall.context, {
      expectedBuddyId: 'b-a',
      expectedBuddiesRevision: 5,
    });
    const secondVersion = instance.api.managementSync.value.version;
    secondCall.resolve({
      snapshot: committedModelSnapshot(4, 5, secondVersion),
      error: null,
    });
    await second;
    await tick();
    assert.equal(instance.api.modelForm.apiKeyInput, '');
    assert.equal(instance.api.modelForm.persona, '苔 A 已应用');
    assert.equal(instance.api.modelPanelDirty.value, false);
  } finally {
    instance.unmount();
  }
});

test('同伙伴修订推进时旧草稿队列不得自动改用新修订派发，正文保留', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    const dispatchCount = instance.calls.length;
    instance.api.onDraftInput({ target: { value: '旧修订未确认草稿' }, isComposing: false });
    await tick();
    assert.equal(instance.calls.length, dispatchCount + 1);
    assert.deepEqual(instance.lastCall().context, {
      expectedBuddyId: 'b-a',
      expectedBuddiesRevision: 4,
    });
    const oldCall = instance.lastCall();

    // 同一伙伴修订推进：旧队列取消，不自动把待发正文换成 r5。
    await load(instance, snapshot(2, { buddiesRevision: 5 }));
    assert.equal(instance.calls.length, dispatchCount + 1);
    assert.equal(instance.api.draft.value, '旧修订未确认草稿');
    oldCall.resolve({ snapshot: snapshot(2, { buddiesRevision: 5, chat: { draft: '旧修订未确认草稿' } }), error: null });
    await tick();
    await tick();
    assert.equal(instance.calls.length, dispatchCount + 1, 'old queue must not dispatch under new revision');

    // 用户在新 context 继续编辑才重新同步。
    instance.api.onDraftInput({ target: { value: '新修订草稿' }, isComposing: false });
    await tick();
    assert.equal(instance.calls.length, dispatchCount + 2);
    assert.deepEqual(instance.lastCall().context, {
      expectedBuddyId: 'b-a',
      expectedBuddiesRevision: 5,
    });
    assert.equal(instance.lastCall().command.text, '新修订草稿');
  } finally {
    for (const call of instance.calls) {
      if (typeof call.resolve === 'function') {
        call.resolve({ snapshot: snapshot(3), error: null });
      }
    }
    instance.unmount();
  }
});

for (const operation of ['create_buddy', 'rename_buddy']) {
  for (const waitingFor of ['chat', 'management']) {
    test(`${operation}在${waitingFor}同步等待中新编辑时取消旧提交并保稿，显式重存只提交新正文`, async () => {
      const instance = component({ deferManagement: waitingFor === 'management' });
      const kind = operation === 'create_buddy' ? 'buddy_create' : 'buddy_rename';
      const submit = () => operation === 'create_buddy'
        ? instance.api.submitBuddyCreate()
        : instance.api.submitBuddyRename();
      const formLabel = () => operation === 'create_buddy'
        ? instance.api.buddyForm.label
        : instance.api.buddyRenameLabel.value;
      const updateForm = (label, persona) => {
        if (operation === 'create_buddy') {
          instance.api.buddyForm.label = label;
          instance.api.buddyForm.persona = persona;
        } else {
          instance.api.buddyRenameLabel.value = label;
        }
      };
      const resolveManagement = async () => {
        for (let round = 0; round < 4; round += 1) {
          for (const call of instance.managementCalls) {
            if (call.resolve) call.resolve({ version: call.version, dirty: call.dirty });
          }
          await tick();
        }
      };
      try {
        await load(instance, snapshot(1));
        if (operation === 'create_buddy') instance.api.openBuddyCreate();
        else instance.api.openBuddyRename();
        updateForm('first name', 'first persona');
        await tick();
        let draftCall;
        if (waitingFor === 'chat') {
          instance.api.onDraftInput({ target: { value: 'held chat draft' }, isComposing: false });
          await tick();
          draftCall = instance.lastCall();
          assert.equal(draftCall.command.kind, 'set_draft');
        }
        const submittedVersion = instance.api.managementSync.value.version;
        submit();
        await tick();
        assert.equal(instance.calls.filter((call) => call.command.kind === operation).length, 0);
        updateForm('new name', 'new persona');
        await tick();
        assert.ok(instance.api.managementSync.value.version > submittedVersion);
        if (draftCall) {
          draftCall.resolve({ snapshot: snapshot(2, {
            chat: { draft: 'held chat draft' },
            managementDraft: { version: instance.api.managementSync.value.version, dirty: true, kind },
          }), error: null });
        }
        await resolveManagement();
        assert.equal(instance.calls.filter((call) => call.command.kind === operation).length, 0,
          '旧正文不能携带新编辑的管理版本提交');
        assert.equal(formLabel(), 'new name');
        if (operation === 'create_buddy') assert.equal(instance.api.buddyForm.persona, 'new persona');
        assert.equal(instance.api.managementSync.value.dirty, true);
        assert.equal(instance.api.managementSync.value.kind, kind);

        submit();
        await resolveManagement();
        const saved = instance.calls.find((call) => call.command.kind === operation);
        assert.ok(saved, '用户显式重存新稿应正常派发');
        assert.equal(saved.command.label, 'new name');
        if (operation === 'create_buddy') assert.equal(saved.command.persona, 'new persona');
        assert.equal(saved.command.expectedManagementDraftVersion, instance.api.managementSync.value.version);
        const version = saved.command.expectedManagementDraftVersion;
        const after = operation === 'create_buddy'
          ? createCommitSnapshot(version, {
              revision: 4, presentationRevision: 4, activeBuddyPersona: 'new persona',
              buddies: [...BUDDIES, buddyView('b-c', 'new name')],
            })
          : snapshot(4, {
              buddiesRevision: 5,
              buddies: [buddyView('b-a', 'new name', { profileRevision: 2 }), BUDDIES[1]],
              managementDraft: { version, dirty: false, kind: 'none' },
              buddyCommit: {
                kind: operation, fromBuddyId: 'b-a', fromRevision: 4,
                toBuddyId: 'b-a', toRevision: 5, managementVersion: version,
              },
            });
        saved.resolve({ snapshot: after, error: null });
        await tick();
        await tick();
        assert.equal(operation === 'create_buddy' ? instance.api.buddyCreateOpen.value : instance.api.buddyRenameOpen.value, false);
        assert.equal(instance.api.managementSync.value.dirty, false);
      } finally {
        instance.unmount();
        for (const call of instance.managementCalls) {
          if (call.resolve) call.resolve({ version: call.version, dirty: call.dirty });
        }
        for (const call of instance.calls) call.resolve({ snapshot: snapshot(9), error: null });
      }
    });
  }
}

test('伙伴动作在草稿同步等待边界复验完整 context：r4 意图不能借 r5 派发', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    // 先制造一个在途草稿同步，让 runBuddyAction 停在第一个同步等待边界。
    instance.api.onDraftInput({ target: { value: '等待同步的草稿' }, isComposing: false });
    await tick();
    const draftCall = instance.lastCall();
    assert.equal(draftCall.command.kind, 'set_draft');
    assert.deepEqual(draftCall.context, {
      expectedBuddyId: 'b-a',
      expectedBuddiesRevision: 4,
    });
    const dispatchCount = instance.calls.length;
    void instance.api.switchToBuddy('b-b');
    await tick();

    // 草稿回包按发起 context 成功结算，随后宿主选择修订推进到 r5。
    // 微任务顺序让 r5 在 runBuddyAction 恢复前生效，复现“等待后重读最新修订”。
    draftCall.resolve({
      snapshot: snapshot(2, { chat: { draft: '等待同步的草稿' } }),
      error: null,
    });
    queueMicrotask(() => {
      instance.api.applyResponse(
        { snapshot: snapshot(3, { buddiesRevision: 5, chat: { draft: '等待同步的草稿' } }), error: null },
        'load',
      );
    });
    await tick();
    await tick();

    assert.equal(instance.api.activeBuddyId.value, 'b-a', 'r5 推进不应切换伙伴');
    assert.equal(
      instance.calls.filter((call) => call.command.kind === 'switch_buddy').length,
      0,
      'r4 发起的切换意图不能在等待后改用 r5 派发',
    );
    assert.equal(instance.calls.length, dispatchCount);
    assert.match(instance.api.lastError.value ?? '', /同步期间发生变化/);
    assert.equal(instance.api.buddyError.value, null);
  } finally {
    instance.unmount();
  }
});

test('伙伴动作在管理编辑同步等待边界复验完整 context：等待期间编辑也不能借 r5 保存', async () => {
  const instance = component({ deferManagement: true });
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '第一份创建稿';
    instance.api.buddyForm.persona = '第一份人设';
    await tick();
    const firstProtection = instance.managementCalls[instance.managementCalls.length - 1];
    firstProtection.resolve({ version: firstProtection.version, dirty: firstProtection.dirty });
    await tick();
    await tick();

    // 在途草稿再次制造第一个等待边界。
    instance.api.onDraftInput({ target: { value: '管理等待边界草稿' }, isComposing: false });
    await tick();
    const draftCall = instance.lastCall();
    instance.api.submitBuddyCreate();
    await tick();
    // 提交后在草稿等待期间继续编辑，使管理同步出现第二个等待边界。
    instance.api.buddyForm.label = '第二份创建稿';
    await tick();
    const heldProtection = instance.managementCalls[instance.managementCalls.length - 1];
    assert.ok(heldProtection.version > firstProtection.version);

    draftCall.resolve({
      snapshot: snapshot(2, { chat: { draft: '管理等待边界草稿' } }),
      error: null,
    });
    await tick();
    heldProtection.resolve({ version: heldProtection.version, dirty: heldProtection.dirty });
    // 管理同步等待期间推进选择修订：无论 flush 先结算还是修订先到，都必须取消，
    // 不能把 r4 发起的创建意图改用 r5 派发。
    queueMicrotask(() => {
      instance.api.applyResponse(
        { snapshot: snapshot(3, { buddiesRevision: 5 }), error: null },
        'load',
      );
    });
    await tick();
    await tick();
    await tick();

    assert.equal(instance.api.activeBuddyId.value, 'b-a');
    assert.equal(
      instance.calls.filter((call) => call.command.kind === 'create_buddy').length,
      0,
      '在 r4 发起、等待期间推进到 r5 的创建意图不得用 r5 派发',
    );
    assert.equal(instance.api.buddyForm.label, '第二份创建稿', '等待期间的编辑不得丢稿');
    assert.equal(instance.api.buddyForm.persona, '第一份人设');
    assert.match(
      instance.api.lastError.value ?? '',
      /管理编辑同步期间发生变化|管理编辑状态尚未同步|表单有新编辑/,
    );
  } finally {
    for (const call of instance.managementCalls) {
      if (typeof call.resolve === 'function') {
        call.resolve({ version: call.version, dirty: call.dirty });
      }
    }
    for (const call of instance.calls) {
      if (typeof call.resolve === 'function') {
        call.resolve({ snapshot: snapshot(4), error: null });
      }
    }
    instance.unmount();
  }
});

function createCommitSnapshot(submittedVersion, overrides = {}) {
  return snapshot(2, {
    activeBuddyId: 'b-c',
    buddiesRevision: 5,
    activeBuddyPersona: '第一份人设',
    buddies: [...BUDDIES, buddyView('b-c', '第一份创建稿')],
    activeSessionId: 's-new',
    sessions: [session('s-new', '新会话')],
    chatDraft: { revision: 1, discard: null, sessions: [] },
    modelSettings: null,
    managementDraft: { version: submittedVersion, dirty: false, kind: 'none' },
    buddyCommit: {
      kind: 'create_buddy',
      fromBuddyId: 'b-a',
      fromRevision: 4,
      toBuddyId: 'b-c',
      toRevision: 5,
      managementVersion: submittedVersion,
    },
    ...overrides,
  });
}

test('创建成功事件先到：提交后新增名称/人设不被 adoptBuddyScope 清掉，回复重复也不丢稿', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '第一份创建稿';
    instance.api.buddyForm.persona = '第一份人设';
    await tick();
    await tick();
    const submittedVersion = instance.managementCalls[instance.managementCalls.length - 1].version;
    instance.api.submitBuddyCreate();
    await tick();
    const createCall = instance.lastCall();
    assert.equal(createCall.command.kind, 'create_buddy');

    // 提交成功回包之前用户继续编辑：这份新稿属于未提交编辑，必须保留。
    instance.api.buddyForm.label = '提交后新名称';
    instance.api.buddyForm.persona = '提交后人设';
    await tick();
    await tick();
    const newerProtection = instance.managementCalls[instance.managementCalls.length - 1];
    assert.ok(newerProtection.version > submittedVersion);

    const created = createCommitSnapshot(submittedVersion);
    const createDispatches = () =>
      instance.calls.filter((call) => call.command.kind === 'create_buddy').length;
    assert.equal(createDispatches(), 1);

    // 快照事件先到：adoptBuddyScope 不得先清掉提交后新增的正文。
    await load(instance, created);
    assert.equal(instance.api.activeBuddyId.value, 'b-c');
    assert.equal(instance.api.buddyCreateOpen.value, true, '新编辑表单不能被事件清掉');
    assert.equal(instance.api.buddyForm.label, '提交后新名称');
    assert.equal(instance.api.buddyForm.persona, '提交后人设');
    assert.equal(instance.api.managementSync.value.dirty, true);
    assert.equal(instance.api.managementSync.value.kind, 'buddy_create');
    const reprotected = instance.managementCalls[instance.managementCalls.length - 1];
    assert.deepEqual(reprotected.context, {
      expectedBuddyId: 'b-c',
      expectedBuddiesRevision: 5,
    });
    assert.equal(createDispatches(), 1, '保留的更新稿不能被自动应用到另一伙伴');

    // 调用回复随后到：同一 buddyCommit 只消费一次，不再次清稿。
    createCall.resolve({ snapshot: created, error: null });
    await tick();
    await tick();
    assert.equal(instance.api.buddyForm.label, '提交后新名称');
    assert.equal(instance.api.buddyForm.persona, '提交后人设');
    assert.equal(instance.api.buddyCreateOpen.value, true);
    assert.match(instance.api.lastError.value ?? '', /新编辑/);
    assert.equal(createDispatches(), 1);

    // 同一快照重复事件也不能清稿或重复登记。
    const protectionCount = instance.managementCalls.length;
    await load(instance, created);
    assert.equal(instance.api.buddyForm.label, '提交后新名称');
    assert.equal(instance.api.buddyForm.persona, '提交后人设');
    assert.equal(instance.managementCalls.length, protectionCount);

    // 用户显式取消：更新稿丢弃，退出保护回到 clean/none，不自动应用。
    instance.api.cancelBuddyCreate();
    await tick();
    await tick();
    assert.equal(instance.api.buddyCreateOpen.value, false);
    assert.equal(instance.api.buddyForm.label, '');
    assert.equal(instance.api.buddyForm.persona, '');
    const cleaned = instance.managementCalls[instance.managementCalls.length - 1];
    assert.equal(cleaned.dirty, false);
    assert.equal(cleaned.kind, 'none');
    assert.equal(createDispatches(), 1);
  } finally {
    for (const call of instance.managementCalls) {
      if (typeof call.resolve === 'function') {
        call.resolve({ version: call.version, dirty: call.dirty });
      }
    }
    for (const call of instance.calls) {
      if (typeof call.resolve === 'function') {
        call.resolve({ snapshot: snapshot(4), error: null });
      }
    }
    instance.unmount();
  }
});

test('创建成功回复先到：同样按 buddyCommit 与本地编辑代际保稿，迟到事件不清新编辑', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '回复先到创建稿';
    instance.api.buddyForm.persona = '回复先到人设';
    await tick();
    await tick();
    const submittedVersion = instance.managementCalls[instance.managementCalls.length - 1].version;
    instance.api.submitBuddyCreate();
    await tick();
    const createCall = instance.lastCall();
    instance.api.buddyForm.label = '回复前新名称';
    instance.api.buddyForm.persona = '回复前新人设';
    await tick();
    await tick();
    const created = createCommitSnapshot(submittedVersion);

    createCall.resolve({ snapshot: created, error: null });
    await tick();
    await tick();
    assert.equal(instance.api.activeBuddyId.value, 'b-c');
    assert.equal(instance.api.buddyCreateOpen.value, true);
    assert.equal(instance.api.buddyForm.label, '回复前新名称');
    assert.equal(instance.api.buddyForm.persona, '回复前新人设');
    assert.match(instance.api.lastError.value ?? '', /新编辑/);

    // 迟到的事件快照与回复相同：不得把已保留的新编辑清掉。
    await load(instance, created);
    assert.equal(instance.api.buddyForm.label, '回复前新名称');
    assert.equal(instance.api.buddyForm.persona, '回复前新人设');
    assert.equal(instance.api.buddyCreateOpen.value, true);
  } finally {
    for (const call of instance.managementCalls) {
      if (typeof call.resolve === 'function') {
        call.resolve({ version: call.version, dirty: call.dirty });
      }
    }
    for (const call of instance.calls) {
      if (typeof call.resolve === 'function') {
        call.resolve({ snapshot: snapshot(4), error: null });
      }
    }
    instance.unmount();
  }
});

test('创建成功事件先到且无新编辑：正常撤表单，重复回复不重复清稿', async () => {
  const instance = component();
  try {
    await load(instance, snapshot(1));
    instance.api.openBuddyCreate();
    instance.api.buddyForm.label = '一次创建稿';
    instance.api.buddyForm.persona = '一次创建人设';
    await tick();
    await tick();
    const submittedVersion = instance.managementCalls[instance.managementCalls.length - 1].version;
    instance.api.submitBuddyCreate();
    await tick();
    const createCall = instance.lastCall();
    const created = createCommitSnapshot(submittedVersion);
    await load(instance, created);
    assert.equal(instance.api.activeBuddyId.value, 'b-c');
    assert.equal(instance.api.buddyCreateOpen.value, false);
    assert.equal(instance.api.buddyForm.label, '');
    createCall.resolve({ snapshot: created, error: null });
    await tick();
    await tick();
    assert.equal(instance.api.buddyCreateOpen.value, false);
    assert.equal(instance.api.buddyForm.label, '');
    assert.match(instance.api.buddyNotice.value ?? '', /已创建/);
  } finally {
    for (const call of instance.managementCalls) {
      if (typeof call.resolve === 'function') {
        call.resolve({ version: call.version, dirty: call.dirty });
      }
    }
    for (const call of instance.calls) {
      if (typeof call.resolve === 'function') {
        call.resolve({ snapshot: snapshot(4), error: null });
      }
    }
    instance.unmount();
  }
});
