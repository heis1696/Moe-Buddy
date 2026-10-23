const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');

// 六页导航回归：执行真实 MainView <script setup>，用受控 IPC 完成回执驱动
// Hide/Open。组件级逻辑与回执语义在这里验证；真实 DOM 呈现由 native smoke 覆盖。
function component() {
  const sourcePath = path.join(__dirname, '../src/app/MainView.vue');
  const source = fs.readFileSync(sourcePath, 'utf8').match(/<script setup lang="ts">([\s\S]*?)<\/script>/)[1];
  const script = ts.transpileModule(source + `
    module.exports.probe = {
      applyResponse, requestPage, cancelPanelLeave, confirmPanelLeave, onDraftInput,
      initialize, lastError, setError, markCommandUnknown, commandUnconfirmed,
      requestRevoke, confirmRevoke, canRequestRevoke,
      activePage, activePanel, panelBusy, pageNavigating,
      pendingPanelLeave, pendingLeavePage,
      draft, rememberDraft, memoryDraft, memoryEdit,
      buddyCreateOpen, buddyRenameOpen, buddyForm, buddyRenameLabel,
      revokeArmed, snapshot, snapshots,
    };
  `, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;

  const dispatches = [];
  let refreshResponse;
  const beforeUnmount = [];
  const scope = vue.effectScope();
  const exported = { exports: {} };
  const context = {
    module: exported,
    exports: exported.exports,
    setTimeout,
    clearTimeout,
    document: { documentElement: { dataset: {} }, getElementById: () => null },
    require(name) {
      if (name === 'vue') {
        return {
          ...vue,
          onMounted() {},
          onBeforeUnmount(callback) { beforeUnmount.push(callback); },
        };
      }
      if (name === '../platform/tauri') {
        return {
          dispatchPrototype: (command, expectation) =>
            new Promise((resolve, reject) => {
              dispatches.push({ command, expectation, resolve, reject });
            }),
          updateManagementDraftState: async (version, dirty, kind) => ({ version, dirty, kind }),
          listenSnapshotEvents: async () => () => {},
          listenFrameEvents: async () => () => {},
          requestSnapshot: async () => refreshResponse,
        };
      }
      if (name === './MaterialPanel.vue') return {};
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
    dispatches,
    refreshWith(response) { refreshResponse = response; },
    reply(index, payload) {
      dispatches[index].resolve(payload);
    },
    async flush() {
      await new Promise((resolve) => setImmediate(resolve));
      await new Promise((resolve) => setImmediate(resolve));
    },
    unmount() {
      if (unmounted) return;
      unmounted = true;
      for (const callback of beforeUnmount) callback();
      scope.stop();
    },
  };
}

function makeSnapshot(revision, overrides = {}) {
  return {
    revision, presentationRevision: revision, mode: 'offline', revoked: false,
    chat: { draft: '', messages: [], status: 'ready', failure: null },
    closeConfirmation: false, closeIntent: null,
    managementDraft: { version: 0, dirty: false, kind: 'none' }, managementExit: null,
    managementDiscardedVersion: null,
    chatDraft: { revision: 1, discard: null, sessions: [] },
    activePreset: '', externalTransmissionAllowed: false,
    presets: [], memories: [{ id: 'm1', revision: 1, text: 'synthetic private note' }],
    diagnostics: [], notice: null, modelSettings: null, persistenceEnabled: true,
    activeBuddyId: 'buddy-a', buddiesRevision: 2,
    activeBuddyPersona: '伙伴 A 的合成人设',
    buddyCommit: null,
    buddies: [{ id: 'buddy-a', label: 'A', profileRevision: 1, personaRevision: 1, modelRevision: 1 }],
    activeSessionId: 'sess-1', sessionsRevision: 3,
    sessions: [{ id: 'sess-1', label: '日常', archived: false, status: 'ready' }],
    sessionIsolationAllowed: false,
    ...overrides,
  };
}

const success = (revision, overrides = {}) => ({ snapshot: makeSnapshot(revision, overrides), error: null });
const failure = (code) => ({
  snapshot: null,
  error: { code, message: 'synthetic panel failure', disposition: 'not_executed' },
});

async function booted() {
  const instance = component();
  instance.api.applyResponse({ snapshot: makeSnapshot(1), error: null }, 'load');
  await instance.flush();
  assert.equal(instance.api.activePage.value, 'chat');
  assert.equal(instance.api.activePanel.value, null);
  return instance;
}

/** 依次派发并按序回执：dispatches 中应恰好是预期的命令序列。 */
function assertCommands(instance, expected) {
  assert.equal(
    instance.dispatches.length,
    expected.length,
    `dispatch count mismatch: ${JSON.stringify(instance.dispatches.map((d) => d.command))}`,
  );
  instance.dispatches.forEach((entry, index) => {
    assert.deepEqual(
      { kind: entry.command.kind, panel: entry.command.panel },
      expected[index],
      `dispatch #${index} mismatch`,
    );
  });
}

test('六页导航：纯呈现页直接切换，面板页按 Hide/Open 成功回执提交', async () => {
  const instance = await booted();
  try {
    // 纯呈现页：无 IPC。
    instance.api.requestPage('buddies');
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'buddies');
    assert.equal(instance.dispatches.length, 0);
    instance.api.requestPage('appearance');
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'appearance');

    // 记忆页：打开面板成功后才提交页面。
    instance.api.requestPage('memory');
    assert.equal(instance.api.activePage.value, 'appearance', '回执前页面不提交');
    instance.reply(0, success(2));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'memory');
    assert.equal(instance.api.activePanel.value, 'memory');

    // 模型页：先隐藏记忆，再打开预设，两步都成功才提交。
    instance.api.requestPage('model');
    instance.reply(1, success(3));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'memory', 'Hide 成功但 Open 未回执时不提交');
    assert.equal(instance.api.activePanel.value, null, 'Hide 成功后面板先关闭，等 Open 回执');
    instance.reply(2, success(4));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'model');
    assert.equal(instance.api.activePanel.value, 'presets');

    // 诊断页：同样先隐藏预设；Open 派发要等 Hide 回执结算后才出现。
    instance.api.requestPage('diagnostics');
    instance.reply(3, success(5));
    await instance.flush();
    instance.reply(4, success(6));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'diagnostics');
    assert.equal(instance.api.activePanel.value, 'diagnostics');

    // 回到纯呈现页：只隐藏当前面板。
    instance.api.requestPage('chat');
    instance.reply(5, success(7));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'chat');
    assert.equal(instance.api.activePanel.value, null);

    assertCommands(instance, [
      { kind: 'open_panel', panel: 'memory' },
      { kind: 'hide_panel', panel: 'memory' },
      { kind: 'open_panel', panel: 'presets' },
      { kind: 'hide_panel', panel: 'presets' },
      { kind: 'open_panel', panel: 'diagnostics' },
      { kind: 'hide_panel', panel: 'diagnostics' },
    ]);
  } finally {
    instance.unmount();
  }
});

test('管理编辑离开确认：取消保留页面与草稿，确认后按回执丢弃并切换', async () => {
  const instance = await booted();
  try {
    instance.api.requestPage('memory');
    instance.reply(0, success(2));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'memory');

    // 聊天草稿走真实输入路径：进入同步队列并由宿主回执收口，与权威正文一致。
    instance.api.onDraftInput({ target: { value: '聊天输入框的合成草稿' }, isComposing: false });
    await instance.flush();
    instance.reply(1, success(3, {
      chat: { draft: '聊天输入框的合成草稿', messages: [], status: 'ready', failure: null },
    }));
    await instance.flush();
    assert.equal(instance.api.draft.value, '聊天输入框的合成草稿');

    instance.api.rememberDraft.value = '尚未保存的合成记忆';
    instance.api.requestPage('chat');
    await instance.flush();
    assert.equal(instance.dispatches.length, 2, '有未保存编辑时不得先派发 Hide');
    assert.notEqual(instance.api.pendingPanelLeave.value, null);
    assert.equal(instance.api.pendingLeavePage.value, 'chat');
    assert.equal(instance.api.activePage.value, 'memory');
    assert.equal(instance.api.activePanel.value, 'memory');
    assert.equal(instance.api.rememberDraft.value, '尚未保存的合成记忆');

    // 取消：页面、面板、管理稿与聊天草稿全部保留。
    instance.api.cancelPanelLeave();
    assert.equal(instance.api.pendingPanelLeave.value, null);
    assert.equal(instance.api.pendingLeavePage.value, null);
    assert.equal(instance.api.activePage.value, 'memory');
    assert.equal(instance.api.rememberDraft.value, '尚未保存的合成记忆');
    assert.equal(instance.api.draft.value, '聊天输入框的合成草稿');

    // 再次导航并确认：Hide 成功回执后面板草稿被清理并提交目标页面；聊天草稿保留。
    instance.api.requestPage('chat');
    assert.notEqual(instance.api.pendingPanelLeave.value, null);
    instance.api.confirmPanelLeave();
    instance.reply(2, success(4, {
      chat: { draft: '聊天输入框的合成草稿', messages: [], status: 'ready', failure: null },
    }));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'chat');
    assert.equal(instance.api.activePanel.value, null);
    assert.equal(instance.api.rememberDraft.value, '', '确认放弃后面板草稿被清理');
    assert.equal(instance.api.draft.value, '聊天输入框的合成草稿', '聊天草稿不属于面板，保留');
    assert.deepEqual(
      instance.dispatches.map((entry) => entry.command.kind),
      ['open_panel', 'set_draft', 'hide_panel'],
    );
  } finally {
    instance.unmount();
  }
});

test('Hide 失败：保留当前页面与面板，导航回到等待状态', async () => {
  const instance = await booted();
  try {
    instance.api.requestPage('model');
    instance.reply(0, success(2));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'model');
    assert.equal(instance.api.activePanel.value, 'presets');

    instance.api.requestPage('chat');
    instance.reply(1, failure('panel_hide_failed'));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'model', 'Hide 失败不得提交目标页面');
    assert.equal(instance.api.activePanel.value, 'presets', 'Hide 失败不得关闭面板');
    assert.equal(instance.api.pageNavigating.value, false);
    assert.equal(instance.api.panelBusy.value, false);
  } finally {
    instance.unmount();
  }
});

test('Hide 成功但 Open 失败：面板已关闭，回退聊天首页并保持跨页错误可见', async () => {
  const instance = await booted();
  try {
    instance.api.requestPage('memory');
    instance.reply(0, success(2));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'memory');

    instance.api.requestPage('model');
    instance.reply(1, success(3));
    await instance.flush();
    assert.equal(instance.api.activePanel.value, null, 'Hide 成功后面板先关闭');
    instance.reply(2, failure('panel_open_failed'));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'chat', '面板已关闭时不得停留在原面板页');
    assert.equal(instance.api.activePanel.value, null);
    assert.equal(instance.api.pageNavigating.value, false);
    assert.ok(instance.api.lastError.value, 'Open 失败必须保留实际错误横幅正文');
  } finally {
    instance.unmount();
  }
});

test('导航期间撤权：撤权快照优先进清理路径，页面回退聊天且不提交目标页', async () => {
  const instance = await booted();
  try {
    instance.api.draft.value = '撤权前正在输入的草稿';
    instance.api.requestPage('memory');
    assert.equal(instance.dispatches.length, 1);
    instance.reply(0, success(2, {
      revoked: true,
      activeBuddyId: null,
      buddiesRevision: 2,
      buddies: [],
      activeBuddyPersona: null,
      buddyCommit: null,
      activeSessionId: null,
      sessionsRevision: 2,
      sessions: [],
      sessionIsolationAllowed: false,
      chatDraft: { revision: 1, discard: null, sessions: [] },
    }));
    await instance.flush();
    assert.equal(instance.api.snapshot.value.revoked, true);
    assert.equal(instance.api.activePanel.value, null, '撤权后面板关闭');
    assert.equal(instance.api.activePage.value, 'chat', '撤权时页面回到中性首页，不提交目标页');
    assert.equal(instance.api.draft.value, '', '撤权清空敏感草稿');
    assert.equal(instance.api.pageNavigating.value, false);

    // 撤权后面板页不可再进入：不派发任何命令。
    const before = instance.dispatches.length;
    instance.api.requestPage('model');
    await instance.flush();
    assert.equal(instance.dispatches.length, before, '撤权后不得打开管理面板');

    // 撤权后纯呈现页仍可导航。
    instance.api.requestPage('buddies');
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'buddies');
  } finally {
    instance.unmount();
  }
});

test('导航期间迟到回包与连续导航：不得串页、不得重复派发', async () => {
  const instance = await booted();
  try {
    instance.api.requestPage('memory');
    assert.equal(instance.dispatches.length, 1);

    // 在途回执之外先到达一份无关快照事件：页面不得因此切换。
    instance.api.applyResponse({ snapshot: makeSnapshot(2), error: null }, 'load');
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'chat', '无关快照不得代替 Open 回执提交页面');

    // 导航在途时的新导航被忽略：不派发第二条命令。
    instance.api.requestPage('buddies');
    assert.equal(instance.dispatches.length, 1, '导航期间连续点击不得再次派发');

    instance.reply(0, success(3));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'memory', '最终按本次 Open 成功回执提交');
    assert.equal(instance.api.activePanel.value, 'memory');
    assert.equal(instance.api.pageNavigating.value, false);

    // 迟到的重复回执不再造成任何页面变化。
    const pageBefore = instance.api.activePage.value;
    instance.api.applyResponse({ snapshot: makeSnapshot(3), error: null }, 'load');
    await instance.flush();
    assert.equal(instance.api.activePage.value, pageBefore);
  } finally {
    instance.unmount();
  }
});

test('聊天草稿与伙伴新建/改名表单跨页保留', async () => {
  const instance = await booted();
  try {
    instance.api.draft.value = '正在写的合成聊天草稿';
    instance.api.buddyCreateOpen.value = true;
    instance.api.buddyForm.label = '小新';
    instance.api.buddyForm.persona = '合成的新伙伴人设';
    instance.api.buddyRenameOpen.value = true;
    instance.api.buddyRenameLabel.value = '改名中的合成名称';

    instance.api.requestPage('buddies');
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'buddies');
    instance.api.requestPage('appearance');
    await instance.flush();
    instance.api.requestPage('chat');
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'chat');
    assert.equal(instance.dispatches.length, 0, '纯呈现页之间往返不派发命令');

    assert.equal(instance.api.draft.value, '正在写的合成聊天草稿');
    assert.equal(instance.api.buddyCreateOpen.value, true);
    assert.equal(instance.api.buddyForm.label, '小新');
    assert.equal(instance.api.buddyForm.persona, '合成的新伙伴人设');
    assert.equal(instance.api.buddyRenameOpen.value, true);
    assert.equal(instance.api.buddyRenameLabel.value, '改名中的合成名称');
  } finally {
    instance.unmount();
  }
});

test('重新读取后退出失配的管理页，侧栏可以重新打开同一面板', async () => {
  const instance = await booted();
  try {
    instance.api.requestPage('model');
    instance.reply(0, success(2));
    await instance.flush();
    instance.refreshWith(success(3));
    await instance.api.initialize();
    assert.equal(instance.api.activePanel.value, null);
    assert.equal(instance.api.activePage.value, 'chat');
    instance.api.requestPage('model');
    assert.equal(instance.dispatches[1].command.kind, 'open_panel');
    instance.reply(1, success(4));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'model');
    assert.equal(instance.api.activePanel.value, 'presets');
  } finally { instance.unmount(); }
});

test('同页导航在面板缺失时重新鉴权打开，不吞掉恢复请求', async () => {
  const instance = await booted();
  try {
    // 模拟页面选择尚在、业务面板已因作用域清场失效的恢复入口。
    instance.api.activePage.value = 'memory';
    instance.api.activePanel.value = null;
    instance.api.requestPage('memory');
    assert.equal(instance.dispatches.length, 1);
    assert.equal(instance.dispatches[0].command.panel, 'memory');
    instance.reply(0, success(2));
    await instance.flush();
    assert.equal(instance.api.activePanel.value, 'memory');
  } finally { instance.unmount(); }
});

test('纯呈现页面切换保留具体 Unknown 错误及锁，不派发业务命令', async () => {
  const instance = await booted();
  try {
    instance.api.markCommandUnknown('send', 'ipc_reply_lost', 'sess-1', 'buddy-a');
    instance.api.setError('发送回包丢失；结果未知，不能自动重发。', null);
    const before = instance.api.lastError.value;
    for (const page of ['buddies', 'appearance', 'chat']) {
      instance.api.requestPage(page);
      await instance.flush();
      assert.equal(instance.api.activePage.value, page);
      assert.equal(instance.api.lastError.value, before);
      assert.equal(instance.api.commandUnconfirmed.value, true);
      assert.equal(instance.dispatches.length, 0);
    }
  } finally { instance.unmount(); }
});

test('注册表 Unknown 拒绝打开诊断后仍可全局确认撤权', async () => {
  const instance = await booted();
  try {
    instance.api.requestPage('buddies');
    await instance.flush();
    instance.api.markCommandUnknown('create_buddy', 'business_unknown', null, 'buddy-a');
    instance.api.requestPage('diagnostics');
    instance.reply(0, failure('buddy_storage_unknown'));
    await instance.flush();
    assert.equal(instance.api.activePage.value, 'buddies');
    assert.equal(instance.api.activePanel.value, null);
    assert.equal(instance.api.commandUnconfirmed.value, true);
    assert.equal(instance.api.canRequestRevoke.value, true);
    instance.api.requestRevoke();
    assert.equal(instance.api.revokeArmed.value, true);
    const revoking = instance.api.confirmRevoke();
    assert.equal(instance.dispatches[1].command.kind, 'revoke');
    assert.equal(instance.dispatches[1].expectation.expectedBuddyId, 'buddy-a');
    instance.reply(1, success(3, {
      revoked: true, activeBuddyId: null, buddies: [], activeBuddyPersona: null,
      activeSessionId: null, sessions: [], memories: [],
    }));
    await revoking;
    assert.equal(instance.api.snapshot.value.revoked, true);
    assert.equal(instance.api.activePage.value, 'chat');
    assert.equal(instance.api.canRequestRevoke.value, false);
  } finally { instance.unmount(); }
});

test('模板契约：六个页面、当前页状态与独立于面板的全局撤权入口', () => {
  const source = fs.readFileSync(path.join(__dirname, '../src/app/MainView.vue'), 'utf8');
  const start = source.indexOf('<template>');
  const end = source.lastIndexOf('</template>');
  assert.ok(start >= 0 && end > start, 'MainView must keep a top-level template block');
  const template = source.slice(start, end);
  for (const page of ['chat', 'buddies', 'appearance', 'memory', 'model', 'diagnostics']) {
    assert.ok(
      template.includes(`data-page-view="${page}"`),
      `missing data-page-view for ${page}`,
    );
    assert.ok(
      template.includes(`:hidden="activePage !== '${page}'"`),
      `page ${page} must toggle visibility with the committed page state`,
    );
  }
  assert.ok(template.includes(':data-page="page"'), 'nav buttons must carry data-page');
  assert.ok(template.includes(`:aria-current="activePage === page ? 'page' : undefined"`),
    'nav buttons must expose aria-current');
  assert.ok(template.includes('aria-labelledby="page-chat-title"'), 'pages need accessible names');
  const sidebar = template.slice(template.indexOf('<aside class="sidebar"'), template.indexOf('</aside>'));
  assert.ok(sidebar.includes('data-testid="revoke-authorization"'), '撤权入口必须在始终可见的侧栏');
  assert.ok(sidebar.includes('@click="requestRevoke"'), '全局按钮必须进入正式撤权确认链');
});
