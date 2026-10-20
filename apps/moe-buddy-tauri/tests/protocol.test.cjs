const test = require('node:test');
const assert = require('node:assert/strict');

const {
  IPC,
  PANELS,
  SESSION_LIFECYCLE_COMMANDS,
  WINDOW_COMMANDS,
  archiveSessionCommand,
  closeChatCommand,
  configureModelCommand,
  cancelManagementExitCommand,
  confirmCloseCommand,
  confirmManagementDiscardCommand,
  deleteMemoryCommand,
  dispatchSourceAllowed,
  hideChatCommand,
  hidePanelCommand,
  newSessionCommand,
  openPanelCommand,
  refreshDiagnosticsCommand,
  refreshMemoriesCommand,
  rememberCommand,
  revokeCommand,
  searchMemoriesCommand,
  selectPresetCommand,
  sendCommand,
  sessionLifecycleAllowed,
  setDraftCommand,
  switchSessionCommand,
  updateMemoryCommand,
  windowCommandAllowed,
  windowCommandPayload,
} = require('../.test-build/protocol.js');
const { IPC_CONTRACT } = require('../.test-build/contract.js');
const { resolveWindowPage } = require('../.test-build/windowLabel.js');

test('冻结的 IPC 命令与事件名不被前端改写', () => {
  assert.equal(IPC.snapshot, 'prototype_snapshot');
  assert.equal(IPC.dispatch, 'prototype_dispatch');
  assert.equal(IPC.window, 'prototype_window');
  assert.equal(IPC.setHitMask, 'prototype_set_hit_mask');
  assert.equal(IPC.frame, 'prototype_frame');
  assert.equal(IPC.snapshotEvent, 'prototype://snapshot');
  assert.equal(IPC.frameEvent, 'prototype://frame');
  assert.equal(IPC.hitMaskErrorEvent, 'prototype://hit-mask-error');
  assert.equal(IPC.managementDraftState, 'prototype_management_draft_state');
  assert.equal(IPC.materialState, 'prototype_material_state');
  assert.equal(IPC.materialCommand, 'prototype_material_command');
  assert.equal(IPC.materialPixels, 'prototype_material_pixels');
  assert.equal(IPC.materialEvent, 'prototype://material');
});

const EXPECTED = { expectedSessionId: 'session-a', expectedSessionsRevision: 3 };

test('dispatch 命令负载保持 { kind, ... } 形状', () => {
  assert.deepEqual(setDraftCommand('你好', EXPECTED), {
    kind: 'set_draft',
    text: '你好',
    ...EXPECTED,
  });
  assert.deepEqual(sendCommand(EXPECTED), { kind: 'send', ...EXPECTED });
  assert.deepEqual(hideChatCommand(), { kind: 'hide_chat' });
  assert.deepEqual(closeChatCommand(), { kind: 'close_chat' });
  assert.deepEqual(confirmCloseCommand(false, null), {
    kind: 'confirm_close',
    discard: false,
    expectedIntent: null,
  });
  assert.deepEqual(confirmCloseCommand(true, 'quit_application'), {
    kind: 'confirm_close',
    discard: true,
    expectedIntent: 'quit_application',
  });
  assert.deepEqual(confirmManagementDiscardCommand(7, 'quit_application'), {
    kind: 'confirm_management_discard',
    version: 7,
    intent: 'quit_application',
  });
  assert.deepEqual(cancelManagementExitCommand(7, 'close_chat'), {
    kind: 'cancel_management_exit',
    version: 7,
    intent: 'close_chat',
  });
  assert.deepEqual(revokeCommand(), { kind: 'revoke' });
});

test('prototype_window 接受完整冻结命令集，包含 show_pet/hide_pet', () => {
  assert.deepEqual(WINDOW_COMMANDS, [
    'show_chat',
    'hide_chat',
    'show_pet',
    'hide_pet',
    'toggle_pause',
    'start_dragging',
    'quit',
  ]);
  assert.deepEqual(IPC_CONTRACT.windowContextCommands, ['show_chat', 'hide_chat', 'quit']);
  // 纯呈现命令保持不带 context。
  for (const command of ['show_pet', 'hide_pet', 'toggle_pause']) {
    assert.deepEqual(windowCommandPayload(command, 'main'), { command });
    assert.deepEqual(windowCommandPayload(command, 'pet'), { command });
  }
  // 主窗上下文命令必须携带发起时捕获的完整伙伴上下文，缺上下文明确拒绝。
  const expectation = { expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 9 };
  for (const command of ['show_chat', 'hide_chat', 'quit']) {
    assert.deepEqual(windowCommandPayload(command, 'main', expectation), {
      command,
      context: expectation,
    });
    assert.throws(
      () => windowCommandPayload(command, 'main'),
      /initiating buddy context/,
      command,
    );
  }
  // 宠物窗只保留 show_chat 聚焦，不携带伙伴上下文。
  assert.deepEqual(windowCommandPayload('show_chat', 'pet'), { command: 'show_chat' });
  assert.deepEqual(windowCommandPayload('show_chat', 'pet', expectation), {
    command: 'show_chat',
  });
  // 宠物窗不得隐藏聊天或退出应用。
  assert.throws(() => windowCommandPayload('hide_chat', 'pet'), /pet window/);
  assert.throws(() => windowCommandPayload('quit', 'pet'), /pet window/);
  // 负载复制捕获值，后续外部修改不污染已构造的命令。
  const captured = { expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 9 };
  const payload = windowCommandPayload('hide_chat', 'main', captured);
  captured.expectedBuddyId = 'buddy-b';
  assert.deepEqual(payload.context, {
    expectedBuddyId: 'buddy-a',
    expectedBuddiesRevision: 9,
  });
});

test('show/hide/pause 两窗均可，拖动仅 pet，退出与隐藏聊天仅 main', () => {
  for (const command of ['show_chat', 'show_pet', 'hide_pet', 'toggle_pause']) {
    assert.equal(windowCommandAllowed('main', command), true, command);
    assert.equal(windowCommandAllowed('pet', command), true, command);
  }
  assert.equal(windowCommandAllowed('main', 'hide_chat'), true);
  assert.equal(windowCommandAllowed('pet', 'hide_chat'), false);
  assert.equal(windowCommandAllowed('pet', 'start_dragging'), true);
  assert.equal(windowCommandAllowed('main', 'start_dragging'), false);
  assert.equal(windowCommandAllowed('main', 'quit'), true);
  assert.equal(windowCommandAllowed('pet', 'quit'), false);
  assert.equal(windowCommandAllowed('settings', 'show_chat'), false);
  assert.equal(windowCommandAllowed(null, 'show_chat'), false);
});

test('快照读取与聊天派发仅 main 来源', () => {
  assert.equal(dispatchSourceAllowed('main'), true);
  assert.equal(dispatchSourceAllowed('pet'), false);
  assert.equal(dispatchSourceAllowed('settings'), false);
  assert.equal(dispatchSourceAllowed(null), false);
});

test('同一 index.html 仅 main/pet 分流，其他 label 不猜测', () => {
  assert.equal(resolveWindowPage('main'), 'main');
  assert.equal(resolveWindowPage('pet'), 'pet');
  assert.equal(resolveWindowPage('settings'), 'unknown');
  assert.equal(resolveWindowPage(null), 'unknown');
  assert.equal(resolveWindowPage(''), 'unknown');
});

test('D 阶段新增命令保持冻结的 { kind, ... } 形状与确切字段名', () => {
  assert.deepEqual(openPanelCommand('presets'), { kind: 'open_panel', panel: 'presets' });
  assert.deepEqual(openPanelCommand('memory'), { kind: 'open_panel', panel: 'memory' });
  assert.deepEqual(openPanelCommand('diagnostics'), { kind: 'open_panel', panel: 'diagnostics' });
  assert.deepEqual(hidePanelCommand('memory'), { kind: 'hide_panel', panel: 'memory' });
  assert.deepEqual(
    configureModelCommand({
      endpoint: 'https://example.invalid/v1',
      model: 'synthetic',
      persona: '苔',
      allowTransmission: false,
      apiKey: null,
    }),
    {
      kind: 'configure_model',
      endpoint: 'https://example.invalid/v1',
      model: 'synthetic',
      persona: '苔',
      allowTransmission: false,
      apiKey: null,
    },
  );
  assert.deepEqual(selectPresetCommand('p1'), { kind: 'select_preset', id: 'p1' });
  assert.deepEqual(rememberCommand('记住 tea'), { kind: 'remember', text: '记住 tea' });
  assert.deepEqual(searchMemoriesCommand('tea'), { kind: 'search_memories', query: 'tea' });
  assert.deepEqual(updateMemoryCommand('7', 3, 'coffee'), {
    kind: 'update_memory',
    id: '7',
    revision: 3,
    text: 'coffee',
  });
  assert.deepEqual(deleteMemoryCommand('7', 3), {
    kind: 'delete_memory',
    id: '7',
    revision: 3,
  });
  assert.deepEqual(refreshMemoriesCommand(), { kind: 'refresh_memories' });
  assert.deepEqual(refreshDiagnosticsCommand(), { kind: 'refresh_diagnostics' });
});

test('panel 取值与冻结夹具一致，新增命令不改变原命令名', () => {
  assert.deepEqual([...PANELS], IPC_CONTRACT.panels);
  assert.deepEqual(IPC_CONTRACT.panels, ['presets', 'memory', 'diagnostics']);
  for (const command of IPC_CONTRACT.dispatchCommands) {
    assert.equal(typeof command, 'string');
  }
  assert.ok(IPC_CONTRACT.dispatchCommands.includes('configure_model'));
  assert.ok(IPC_CONTRACT.dispatchCommands.includes('delete_memory'));
  assert.ok(IPC_CONTRACT.dispatchCommands.includes('refresh_diagnostics'));
  assert.ok(IPC_CONTRACT.dispatchCommands.includes('confirm_management_discard'));
  assert.ok(IPC_CONTRACT.dispatchCommands.includes('cancel_management_exit'));
  assert.deepEqual(IPC_CONTRACT.hostCommands, ['prototype_management_draft_state']);
});

test('configure_model 的 apiKey 只允许字符串或 null', () => {
  const base = {
    endpoint: 'e',
    model: 'm',
    persona: 'p',
    allowTransmission: true,
  };
  assert.equal(configureModelCommand({ ...base, apiKey: null }).apiKey, null);
  assert.equal(configureModelCommand({ ...base, apiKey: '' }).apiKey, '');
  assert.equal(configureModelCommand({ ...base, apiKey: 'secret' }).apiKey, 'secret');
});

test('A 生命周期命令携带预期会话与目录修订，且原字段保持不变', () => {
  assert.deepEqual(newSessionCommand(EXPECTED), { kind: 'new_session', ...EXPECTED });
  assert.deepEqual(switchSessionCommand('session-b', EXPECTED), {
    kind: 'switch_session',
    id: 'session-b',
    ...EXPECTED,
  });
  assert.deepEqual(archiveSessionCommand('session-a', EXPECTED), {
    kind: 'archive_session',
    id: 'session-a',
    ...EXPECTED,
  });
  assert.deepEqual(setDraftCommand('draft', EXPECTED), {
    kind: 'set_draft',
    text: 'draft',
    ...EXPECTED,
  });
  assert.deepEqual(sendCommand(EXPECTED), { kind: 'send', ...EXPECTED });
});

test('A 生命周期命令与快照字段纳入共享夹具，目录只允许 main 业务入口', () => {
  assert.deepEqual(
    SESSION_LIFECYCLE_COMMANDS,
    IPC_CONTRACT.dispatchCommands.filter((command) =>
      ['new_session', 'switch_session', 'archive_session'].includes(command),
    ),
  );
  assert.deepEqual(IPC_CONTRACT.sessionSnapshotFields, [
    'activeSessionId',
    'sessionsRevision',
    'sessions',
  ]);
  assert.deepEqual(IPC_CONTRACT.sessionFields, ['id', 'label', 'archived', 'status']);
  assert.deepEqual(IPC_CONTRACT.sessionStatuses, ['ready', 'sending', 'failed', 'unknown']);
  assert.deepEqual(IPC_CONTRACT.sessionLimits, { idMaxBytes: 64, directoryMax: 64 });
  assert.equal(sessionLifecycleAllowed('main'), true);
  assert.equal(sessionLifecycleAllowed('pet'), false);
  assert.equal(sessionLifecycleAllowed('settings'), false);
  assert.equal(sessionLifecycleAllowed(null), false);
});
