const test = require('node:test');
const assert = require('node:assert/strict');

const {
  BUDDY_LIFECYCLE_COMMANDS,
  IPC,
  buddyLifecycleAllowed,
  archiveSessionCommand,
  cancelManagementExitCommand,
  closeChatCommand,
  configureModelCommand,
  confirmCloseCommand,
  confirmManagementDiscardCommand,
  createBuddyCommand,
  deleteMemoryCommand,
  dispatchContext,
  dispatchEnvelope,
  hideChatCommand,
  hidePanelCommand,
  newSessionCommand,
  openPanelCommand,
  refreshDiagnosticsCommand,
  refreshMemoriesCommand,
  rememberCommand,
  renameBuddyCommand,
  revokeCommand,
  searchMemoriesCommand,
  selectPresetCommand,
  sendCommand,
  setDraftCommand,
  switchBuddyCommand,
  switchSessionCommand,
  updateMemoryCommand,
} = require('../.test-build/protocol.js');
const { IPC_CONTRACT } = require('../.test-build/contract.js');
const { buddyExpectationOf } = require('../.test-build/buddy.js');
const { CommandUnconfirmedLock } = require('../.test-build/refresh.js');
const {
  ManagementDraftSync,
  consumeManagementDiscardReceipt,
} = require('../.test-build/managementDraftSync.js');

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

// 管理同步作用域必须同时比较伙伴 ID 与选择修订；同一伙伴的新修订也是新作用域。
const SCOPE_A = { expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 6 };
const SCOPE_B = { expectedBuddyId: 'buddy-b', expectedBuddiesRevision: 6 };
const SCOPE_A_NEXT = { expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 7 };

const CONTEXT = { expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 7 };
const EXPECTED_SESSION = { expectedSessionId: 's-a', expectedSessionsRevision: 3 };

test('prototype_dispatch 保留 command 并增加必填 context，字段与形状冻结', () => {
  const envelope = dispatchEnvelope(sendCommand(EXPECTED_SESSION), CONTEXT);
  assert.deepEqual(Object.keys(envelope).sort(), ['command', 'context']);
  assert.deepEqual(Object.keys(envelope.context).sort(), [
    'expectedBuddiesRevision',
    'expectedBuddyId',
  ]);
  assert.deepEqual(envelope, {
    command: { kind: 'send', ...EXPECTED_SESSION },
    context: { expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 7 },
  });
  assert.deepEqual(dispatchContext(CONTEXT), {
    expectedBuddyId: 'buddy-a',
    expectedBuddiesRevision: 7,
  });
  // 空串哨兵表示宿主尚未签发伙伴，不是另一个伙伴 ID。
  assert.deepEqual(buddyExpectationOf(null), {
    expectedBuddyId: '',
    expectedBuddiesRevision: 0,
  });
});

test('所有派发命令都能携带同一伙伴上下文，且不修改原命令形状', () => {
  const commands = [
    setDraftCommand('draft', EXPECTED_SESSION),
    sendCommand(EXPECTED_SESSION),
    newSessionCommand(EXPECTED_SESSION),
    switchSessionCommand('s-b', EXPECTED_SESSION),
    archiveSessionCommand('s-a', EXPECTED_SESSION),
    hideChatCommand(),
    closeChatCommand(),
    confirmCloseCommand(false, null),
    confirmManagementDiscardCommand(2, 'quit_application'),
    cancelManagementExitCommand(2, 'close_chat'),
    revokeCommand(),
    openPanelCommand('memory'),
    hidePanelCommand('memory'),
    configureModelCommand({
      endpoint: 'http://127.0.0.1:9',
      model: 'synthetic',
      persona: 'p',
      allowTransmission: false,
      apiKey: null,
    }),
    selectPresetCommand('p1'),
    rememberCommand('tea'),
    searchMemoriesCommand('tea'),
    updateMemoryCommand('1', 2, 'tea'),
    deleteMemoryCommand('1', 2),
    refreshMemoriesCommand(),
    refreshDiagnosticsCommand(),
    createBuddyCommand('新伙伴', '人设', 3),
    switchBuddyCommand('buddy-b'),
    renameBuddyCommand('改名后', 4),
  ];
  const names = new Set(commands.map((command) => command.kind));
  for (const command of IPC_CONTRACT.dispatchCommands) {
    assert.equal(names.has(command), true, `missing dispatch fixture: ${command}`);
  }
  for (const command of commands) {
    const envelope = dispatchEnvelope(command, CONTEXT);
    assert.deepEqual(envelope.command, command);
    assert.deepEqual(envelope.context, {
      expectedBuddyId: 'buddy-a',
      expectedBuddiesRevision: 7,
    });
  }
});

test('B 伙伴命令只接受名称、人设、精确草稿版本与已有 ID，不提供归属/路径/权限字段', () => {
  assert.deepEqual(BUDDY_LIFECYCLE_COMMANDS, ['create_buddy', 'switch_buddy', 'rename_buddy']);
  assert.deepEqual(createBuddyCommand('新伙伴', '有界人设', 3), {
    kind: 'create_buddy',
    label: '新伙伴',
    persona: '有界人设',
    expectedManagementDraftVersion: 3,
  });
  assert.deepEqual(switchBuddyCommand('buddy-b'), { kind: 'switch_buddy', id: 'buddy-b' });
  assert.deepEqual(renameBuddyCommand('改名后', 4), {
    kind: 'rename_buddy',
    label: '改名后',
    expectedManagementDraftVersion: 4,
  });
  for (const command of [
    createBuddyCommand('n', 'p', 1),
    switchBuddyCommand('buddy-b'),
    renameBuddyCommand('n', 2),
  ]) {
    for (const forbidden of ['path', 'root', 'owner', 'permissions', 'inheritFrom', 'apiKey']) {
      assert.equal(forbidden in command, false, `${command.kind} must not expose ${forbidden}`);
    }
  }
  assert.deepEqual(IPC_CONTRACT.buddyCommands, [...BUDDY_LIFECYCLE_COMMANDS]);
  assert.equal(IPC.managementDraftState, 'prototype_management_draft_state');
});

test('伙伴生命周期仍只允许主窗；宠物/设置窗不能借窗口命令绕过业务校验', () => {
  assert.equal(buddyLifecycleAllowed('main'), true);
  assert.equal(buddyLifecycleAllowed('pet'), false);
  assert.equal(buddyLifecycleAllowed('settings'), false);
  assert.equal(buddyLifecycleAllowed(null), false);
  assert.equal(buddyLifecycleAllowed(undefined), false);
});

test('未知锁按 (伙伴, 会话) 隔离：同局部会话 ID 不共享，A→B→A 保留各自未知', () => {
  const lock = new CommandUnconfirmedLock();
  lock.enterScope('buddy-a', 's-shared');
  lock.markUnknown('send', 'business_unknown');
  assert.equal(lock.locked, true);
  assert.equal(lock.hasSessionUnknown, true);
  assert.equal(lock.hasGlobalUnknown, false);

  lock.enterScope('buddy-b', 's-shared');
  assert.equal(lock.locked, false, '另一伙伴的同名局部会话不继承未知');

  lock.markUnknown('model', 'ipc_reply_lost');
  assert.equal(lock.hasGlobalUnknown, true, '全局未知只属于 buddy-b');
  assert.equal(lock.locked, true);

  lock.enterScope('buddy-a', 's-shared');
  assert.equal(lock.locked, true, '切回 buddy-a 仍保留它自己的发送未知');
  assert.equal(lock.hasGlobalUnknown, false, 'buddy-b 的全局未知不串到 buddy-a');
  assert.equal(lock.operation, 'send');

  lock.confirm('send', 's-shared');
  assert.equal(lock.locked, false, '在 buddy-a 作用域确认只清 buddy-a 的会话未知');
  lock.enterScope('buddy-b', 's-shared');
  assert.equal(lock.locked, true, 'buddy-b 的全局未知仍在');
  lock.confirm('model', null);
  assert.equal(lock.locked, false);
});

test('按发起作用域确认不清当前伙伴；撤权迁移全局未知不把未知当成已收口', () => {
  const lock = new CommandUnconfirmedLock();
  lock.enterScope('buddy-a', 's-a');
  lock.markUnknown('send', 'ipc_reply_lost');
  lock.markUnknown('switch_session', 'business_unknown', null);
  lock.enterScope('buddy-b', 's-a');
  lock.confirmScoped('switch_session', 'buddy-a', null);
  assert.equal(lock.hasGlobalUnknown, false, 'buddy-a 的全局未知被自己的回执清除');
  assert.equal(lock.hasSessionUnknown, false, 'buddy-b 从未有会话未知');

  lock.markUnknown('close', 'business_unknown', null, 'buddy-a');
  assert.equal(lock.locked, false, '旧伙伴的全局未知不阻塞新伙伴');
  lock.migrateGlobalUnknown('buddy-a', '');
  lock.enterBuddy('');
  assert.equal(lock.locked, true, '撤权后未知迁到关闭作用域仍保守锁定');
  assert.equal(lock.operation, 'close');
  lock.reset();
  assert.equal(lock.locked, false);
});

test('管理编辑同步绑定完整作用域：ID 或选择修订变化都不能采纳/消费', async () => {
  const sync = new ManagementDraftSync(SCOPE_A, async (version, dirty) => ({ version, dirty }));
  assert.equal(sync.record(true, 'general', SCOPE_B), false, '另一伙伴不能记录编辑');
  assert.equal(sync.record(true, 'general', SCOPE_A_NEXT), false, '同一伙伴的新修订不是旧作用域');
  assert.equal(sync.record(true, 'general', SCOPE_A), true);
  await tick();
  assert.equal(sync.dirty, true);
  assert.equal(sync.adopt({ version: 9, dirty: false, kind: 'none' }, SCOPE_B), false);
  assert.equal(sync.adopt({ version: 9, dirty: false, kind: 'none' }, SCOPE_A_NEXT), false);
  assert.equal(sync.version, 1);

  assert.equal(sync.adopt({ version: 9, dirty: false, kind: 'none' }, SCOPE_A), true);
  assert.equal(sync.version, 9);
  assert.equal(sync.record(true, 'general', SCOPE_A), true);
  await tick();
  assert.equal(sync.consumeDiscardedVersion(sync.version, SCOPE_B), false, '旧伙伴回执不清当前编辑');
  assert.equal(sync.consumeDiscardedVersion(sync.version, SCOPE_A_NEXT), false, '旧修订回执不清当前编辑');
  assert.equal(sync.dirty, true);
  assert.equal(sync.consumeDiscardedVersion(sync.version, SCOPE_A), true);
  assert.equal(sync.dirty, false);

  // 再次产生未保存编辑：另一作用域的回执既不消费也不清稿。
  assert.equal(sync.record(true, 'general', SCOPE_A), true);
  await tick();
  const cleared = [];
  assert.equal(
    consumeManagementDiscardReceipt(
      { managementDiscardedVersion: sync.version },
      sync,
      () => cleared.push('a'),
      SCOPE_B,
    ),
    false,
  );
  assert.equal(
    consumeManagementDiscardReceipt(
      { managementDiscardedVersion: sync.version },
      sync,
      () => cleared.push('a'),
      SCOPE_A_NEXT,
    ),
    false,
  );
  assert.deepEqual(cleared, []);
  assert.equal(sync.dirty, true);
  assert.equal(
    consumeManagementDiscardReceipt(
      { managementDiscardedVersion: sync.version },
      sync,
      () => cleared.push('a'),
      SCOPE_A,
    ),
    true,
  );
  assert.deepEqual(cleared, ['a']);
});

test('选择修订推进时取消旧同步：待发合并值不再派发，迟到回执不写回', async () => {
  const calls = [];
  const sync = new ManagementDraftSync(SCOPE_A, (version, dirty) =>
    new Promise((resolve) => calls.push({ version, dirty, resolve })),
  );
  sync.record(true, 'buddy_create');
  await tick();
  sync.record(true, 'buddy_create');
  assert.equal(calls.length, 1, 'one in-flight plus one merged pending');
  const pendingFlush = sync.flush();
  sync.cancel();
  assert.equal(sync.isCancelled, true);
  assert.equal(sync.isSynced, false);
  assert.equal(await pendingFlush, false);
  assert.equal(sync.record(true, 'buddy_create'), false, '取消后的旧需求不再接受编辑');
  assert.equal(sync.adopt({ version: 3, dirty: true, kind: 'buddy_create' }), false);

  // 旧在途回执到达后：不发第二份，也不把旧作用域状态写成已确认。
  calls[0].resolve({ version: 1, dirty: true });
  await tick();
  await tick();
  assert.equal(calls.length, 1, 'queued old edit must not be dispatched after scope revision advanced');
  assert.equal(sync.dirty, true);
  assert.equal(sync.isSynced, false);
});

test('旧伙伴的丢弃回执不能清或多清当前伙伴的编辑水位', async () => {
  const sync = new ManagementDraftSync(SCOPE_A, async (version, dirty) => ({ version, dirty }));
  assert.equal(sync.record(true, 'general', SCOPE_A), true);
  await tick();
  assert.equal(
    consumeManagementDiscardReceipt(
      { managementDiscardedVersion: 1 },
      sync,
      () => {},
      SCOPE_A,
    ),
    true,
  );
  assert.equal(sync.dirty, false);

  // 新编辑产生更高版本；带着旧作用域的同版本回执不得冒充。
  assert.equal(sync.record(true, 'general', SCOPE_A), true);
  await tick();
  assert.equal(
    consumeManagementDiscardReceipt(
      { managementDiscardedVersion: 2 },
      sync,
      () => {
        throw new Error('must not clear another buddy edits');
      },
      SCOPE_B,
    ),
    false,
  );
  assert.equal(
    consumeManagementDiscardReceipt(
      { managementDiscardedVersion: 2 },
      sync,
      () => {
        throw new Error('must not clear another revision edits');
      },
      SCOPE_A_NEXT,
    ),
    false,
  );
  assert.equal(sync.dirty, true);
  // 同一作用域的旧回执（版本 1）也不能清版本 2 的编辑。
  assert.equal(
    consumeManagementDiscardReceipt(
      { managementDiscardedVersion: 1 },
      sync,
      () => {
        throw new Error('stale receipt must not clear newer edits');
      },
      SCOPE_A,
    ),
    false,
  );
  assert.equal(sync.dirty, true);
});
