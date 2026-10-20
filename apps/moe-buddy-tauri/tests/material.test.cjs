const test = require('node:test');
const assert = require('node:assert/strict');

const {
  MATERIAL_COMMAND_TYPES,
  MATERIAL_LIMITS,
  MATERIAL_PACK_PIXEL_BUDGET_BYTES,
  MATERIAL_SOURCES,
  MATERIAL_STATUSES,
  RevisionOrderedMaterialStates,
  builtinMaterialCommand,
  isLocalMaterialPath,
  isMaterialId,
  isMaterialLabel,
  loadMaterialCommand,
  materialCommandEnvelope,
  materialDropIntent,
  materialDropWithinArea,
  materialFrameTriple,
  materialStateErrorText,
  parseMaterialFrame,
  parseMaterialReply,
  parseMaterialState,
  pauseMaterialCommand,
  playMaterialCommand,
  resumeMaterialCommand,
  sameMaterialTriple,
  stopMaterialCommand,
  unloadMaterialCommand,
} = require('../.test-build/material.js');
const { IPC_CONTRACT } = require('../.test-build/contract.js');

function builtinState(overrides = {}) {
  return {
    revision: 1,
    resourceGeneration: 1,
    playbackGeneration: 1,
    source: 'builtin',
    pack: null,
    actions: [],
    actionId: null,
    status: 'stopped',
    loading: false,
    decodedBytes: 0,
    frameCount: 4,
    error: null,
    ...overrides,
  };
}

function packageState(overrides = {}) {
  return builtinState({
    source: 'package',
    pack: { id: 'synthetic-seq', label: '合成序列', version: 1 },
    actions: [
      { id: 'idle', label: '待机' },
      { id: 'wave', label: '招手' },
    ],
    actionId: 'idle',
    status: 'playing',
    decodedBytes: 4096,
    frameCount: 4,
    ...overrides,
  });
}

function emptyState(overrides = {}) {
  return builtinState({
    source: 'empty',
    frameCount: 0,
    status: 'stopped',
    ...overrides,
  });
}

function builtinFrame(overrides = {}) {
  return {
    resourceGeneration: 1,
    playbackGeneration: 1,
    presentationSequence: 1,
    source: 'builtin',
    clipId: 'idle-four-frame',
    frameIndex: 0,
    width: 320,
    height: 320,
    paused: false,
    faulted: false,
    ...overrides,
  };
}

function packageFrame(overrides = {}) {
  return builtinFrame({
    source: 'package',
    clipId: 'idle',
    frameIndex: 2,
    width: 512,
    height: 512,
    ...overrides,
  });
}

function emptyFrame(overrides = {}) {
  return builtinFrame({
    source: 'empty',
    clipId: null,
    frameIndex: null,
    width: 0,
    height: 0,
    ...overrides,
  });
}

function reply(state, overrides = {}) {
  return { ok: true, state, error: null, ...overrides };
}

function context() {
  return { expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 7 };
}

test('素材状态严格字段、来源/状态枚举、修订与预算，缺字段或多字段都拒绝', () => {
  assert.deepEqual(parseMaterialState(builtinState()), builtinState());
  assert.deepEqual(parseMaterialState(packageState()), packageState());
  assert.deepEqual(parseMaterialState(emptyState()), emptyState());

  for (const bad of [
    { ...builtinState(), extra: true },
    { ...builtinState(), path: 'C:/secret' },
    { ...builtinState(), buddyId: 'buddy-a' },
  ]) {
    assert.equal(parseMaterialState(bad), null);
  }
  assert.equal(parseMaterialState({ ...builtinState(), revision: 0 }), null);
  assert.equal(parseMaterialState({ ...builtinState(), resourceGeneration: -1 }), null);
  assert.equal(parseMaterialState({ ...builtinState(), playbackGeneration: 1.5 }), null);
  assert.equal(parseMaterialState({ ...builtinState(), revision: Number.MAX_SAFE_INTEGER + 1 }), null);
  assert.equal(parseMaterialState({ ...builtinState(), source: 'file' }), null);
  assert.equal(parseMaterialState({ ...builtinState(), status: 'unknown' }), null);
  assert.equal(parseMaterialState({ ...builtinState(), loading: 'no' }), null);
  assert.equal(parseMaterialState({ ...builtinState(), decodedBytes: -1 }), null);
  assert.equal(parseMaterialState({ ...builtinState(), decodedBytes: MATERIAL_LIMITS.decodedMaxBytes + 1 }), null);
  assert.equal(parseMaterialState({ ...builtinState(), frameCount: 513 }), null);
});

test('素材状态的一致性：package 必须有包，empty 必须零资源且动作目录为空', () => {
  assert.equal(parseMaterialState({ ...packageState(), pack: null }), null);
  assert.equal(parseMaterialState({ ...builtinState(), pack: { id: 'x', label: 'X', version: 1 } }), null);
  assert.equal(parseMaterialState({ ...emptyState(), frameCount: 1 }), null);
  assert.equal(parseMaterialState({ ...emptyState(), decodedBytes: 1 }), null);
  assert.equal(parseMaterialState({ ...emptyState(), actionId: 'idle' }), null);
  assert.equal(
    parseMaterialState({ ...emptyState(), actions: [{ id: 'idle', label: '待机' }] }),
    null,
    'empty 表示资源已卸载，动作目录也必须为零',
  );
  assert.equal(parseMaterialState({ ...packageState(), actionId: 'missing' }), null);
  assert.equal(parseMaterialState({ ...packageState(), actions: [{ id: 'idle', label: 'A' }, { id: 'idle', label: 'B' }] }), null);
  assert.equal(
    parseMaterialState({ ...packageState(), actions: [{ id: 'idle', label: '含\u0000控制' }] }),
    null,
  );
  assert.equal(parseMaterialState({ ...packageState(), actions: [{ id: 'bad id', label: 'A' }] }), null);
  assert.equal(parseMaterialState({ ...packageState(), pack: { id: 'synthetic-seq', label: '', version: 1 } }), null);
  assert.equal(parseMaterialState({ ...packageState(), pack: { id: 'synthetic-seq', label: 'A', version: 0 } }), null);
});

test('source 表示已装资源：stop 可保留 package 字节/动作，unload 才是零资源 empty', () => {
  const stopped = packageState({ status: 'stopped' });
  assert.deepEqual(parseMaterialState(stopped), stopped);
  assert.equal(stopped.source, 'package');
  assert.ok(stopped.decodedBytes > 0);
  assert.ok(stopped.actions.length > 0);

  const unloaded = emptyState();
  assert.deepEqual(parseMaterialState(unloaded), unloaded);
  assert.equal(unloaded.pack, null);
  assert.equal(unloaded.actions.length, 0);
  assert.equal(unloaded.actionId, null);
  assert.equal(unloaded.decodedBytes, 0);
  assert.equal(unloaded.frameCount, 0);
  // 停止只让帧快照变为 empty；帧描述本身不得残留 package 的 Clip/尺寸。
  const stoppedFrame = emptyFrame();
  assert.equal(stoppedFrame.source, 'empty');
  assert.equal(stoppedFrame.clipId, null);
  assert.equal(stoppedFrame.frameIndex, null);
  assert.equal(stoppedFrame.width, 0);
  assert.equal(stoppedFrame.height, 0);
});

test('素材错误保持固定安全形状与有界文案；不再凭斜杠猜测是否含路径', () => {
  assert.deepEqual(
    parseMaterialState({ ...packageState(), error: { code: 'pack_rejected', message: '素材包校验失败。' } }).error,
    { code: 'pack_rejected', message: '素材包校验失败。' },
  );
  // 斜杠/反斜杠不再触发前端猜测，宿主固定安全文案按原样展示。
  assert.deepEqual(
    parseMaterialState({ ...packageState(), error: { code: 'x', message: 'C:/secret' } }).error,
    { code: 'x', message: 'C:/secret' },
  );
  assert.deepEqual(
    parseMaterialState({ ...packageState(), error: { code: 'x', message: 'a\\b' } }).error,
    { code: 'x', message: 'a\\b' },
  );
  assert.equal(parseMaterialState({ ...packageState(), error: { code: 'x', message: 'a', path: 'x' } }), null);
  assert.equal(parseMaterialState({ ...packageState(), error: { code: '', message: 'a' } }), null);
  assert.equal(parseMaterialState({ ...packageState(), error: { code: 'x', message: '含\u0000控制' } }), null);
  assert.equal(parseMaterialState({ ...packageState(), error: { code: 'x', message: 'a'.repeat(513) } }), null);
  assert.equal(materialStateErrorText(packageState()), null);
  assert.equal(
    materialStateErrorText({ ...packageState(), error: { code: 'x', message: '固定文案' } }),
    '固定文案',
  );
});

test('帧描述严格必填：builtin 固定 320×320，package 真实 Clip/0..255，empty 显式空', () => {
  assert.deepEqual(parseMaterialFrame(builtinFrame()), builtinFrame());
  assert.deepEqual(parseMaterialFrame(packageFrame()), packageFrame());
  assert.deepEqual(parseMaterialFrame(emptyFrame()), emptyFrame());

  // 旧协议只有 frameIndex/paused/faulted，必须整份拒绝。
  assert.equal(parseMaterialFrame({ frameIndex: 0, paused: false, faulted: false }), null);
  assert.equal(parseMaterialFrame({ ...builtinFrame(), extra: true }), null);
  assert.equal(parseMaterialFrame({ ...builtinFrame(), presentationSequence: 0 }), null);
  assert.equal(parseMaterialFrame({ ...builtinFrame(), source: 'empty' }), null);
  assert.equal(parseMaterialFrame({ ...builtinFrame(), clipId: 'other' }), null);
  assert.equal(parseMaterialFrame({ ...builtinFrame(), frameIndex: 4 }), null);
  assert.equal(parseMaterialFrame({ ...builtinFrame(), width: 321 }), null);
  assert.equal(parseMaterialFrame({ ...packageFrame(), clipId: 'bad id' }), null);
  assert.equal(parseMaterialFrame({ ...packageFrame(), frameIndex: 256 }), null);
  assert.equal(parseMaterialFrame({ ...packageFrame(), frameIndex: -1 }), null);
  assert.equal(parseMaterialFrame({ ...packageFrame(), width: 513 }), null);
  assert.equal(parseMaterialFrame({ ...packageFrame(), height: 0 }), null);
  assert.equal(parseMaterialFrame({ ...emptyFrame(), clipId: 'idle' }), null);
  assert.equal(parseMaterialFrame({ ...emptyFrame(), frameIndex: 0 }), null);
  assert.equal(parseMaterialFrame({ ...emptyFrame(), width: 1 }), null);
  assert.equal(parseMaterialFrame({ ...emptyFrame(), paused: 'no' }), null);
});

test('帧三元组与相等比较只认三个正安全整数', () => {
  assert.deepEqual(materialFrameTriple(packageFrame()), {
    resourceGeneration: 1,
    playbackGeneration: 1,
    presentationSequence: 1,
  });
  assert.equal(sameMaterialTriple(materialFrameTriple(packageFrame()), materialFrameTriple(packageFrame())), true);
  assert.equal(
    sameMaterialTriple(materialFrameTriple(packageFrame()), materialFrameTriple(packageFrame({ presentationSequence: 2 }))),
    false,
  );
  assert.equal(sameMaterialTriple(null, null), true);
  assert.equal(sameMaterialTriple(null, materialFrameTriple(packageFrame())), false);
});

test('素材命令回复严格 { ok, state, error }：成功不得带错误，失败必须带安全 Failure', () => {
  assert.deepEqual(parseMaterialReply(reply(builtinState())), reply(builtinState()));
  const failure = { code: 'material_busy', message: '素材正在加载。', disposition: 'not_executed' };
  assert.deepEqual(
    parseMaterialReply({ ok: false, state: packageState(), error: failure }),
    { ok: false, state: packageState(), error: failure },
  );
  assert.equal(parseMaterialReply({ ...reply(builtinState()), extra: true }), null);
  assert.equal(parseMaterialReply({ ok: true, state: null, error: null }), null);
  assert.equal(parseMaterialReply({ ok: true, state: builtinState(), error: failure }), null);
  assert.equal(parseMaterialReply({ ok: false, state: builtinState(), error: null }), null);
  assert.equal(
    parseMaterialReply({ ok: false, state: builtinState(), error: { ...failure, disposition: 'maybe' } }),
    null,
  );
  assert.equal(parseMaterialReply({ ok: false, state: builtinState() }), null);
});

test('素材命令使用 type 判别，所有命令必含双代际；load 只接受本地绝对路径', () => {
  const state = packageState();
  assert.deepEqual(loadMaterialCommand('C:\\packs\\cat', state), {
    type: 'load',
    path: 'C:\\packs\\cat',
    expectedResourceGeneration: 1,
    expectedPlaybackGeneration: 1,
  });
  assert.deepEqual(loadMaterialCommand('D:/packs/cat/manifest.json', state), {
    type: 'load',
    path: 'D:/packs/cat/manifest.json',
    expectedResourceGeneration: 1,
    expectedPlaybackGeneration: 1,
  });
  assert.deepEqual(playMaterialCommand('wave', state), {
    type: 'play',
    actionId: 'wave',
    expectedResourceGeneration: 1,
    expectedPlaybackGeneration: 1,
  });
  for (const [builder, type] of [
    [pauseMaterialCommand, 'pause'],
    [resumeMaterialCommand, 'resume'],
    [stopMaterialCommand, 'stop'],
    [unloadMaterialCommand, 'unload'],
    [builtinMaterialCommand, 'builtin'],
  ]) {
    assert.deepEqual(builder(state), {
      type,
      expectedResourceGeneration: 1,
      expectedPlaybackGeneration: 1,
    });
  }
  assert.deepEqual(MATERIAL_COMMAND_TYPES, ['load', 'play', 'pause', 'resume', 'stop', 'unload', 'builtin']);
});

test('导入路径拒绝 UNC、设备、相对、非盘符、冒号与控制字符；多路径一次只收一个', () => {
  assert.equal(isLocalMaterialPath('C:\\packs\\cat'), true);
  assert.equal(isLocalMaterialPath('D:/packs/cat/manifest.json'), true);
  for (const bad of [
    '\\\\server\\share\\cat',
    '//server/share/cat',
    '\\\\.\\GLOBALROOT\\cat',
    '\\\\?\\C:\\packs\\cat',
    'C:relative',
    'cat\\manifest.json',
    './cat',
    'C:\\cat:stream',
    'C:\\packs\\cat\u0000',
    'C:\\packs\\..\\secret',
    '',
    42,
    null,
  ]) {
    assert.equal(isLocalMaterialPath(bad), false, String(bad));
  }
  assert.deepEqual(materialDropIntent(['C:\\packs\\cat']), { ok: true, path: 'C:\\packs\\cat' });
  assert.equal(materialDropIntent([]).ok, false);
  assert.equal(materialDropIntent(['C:\\a', 'C:\\b']).ok, false);
  assert.equal(materialDropIntent(['\\\\server\\share']).ok, false);
  assert.equal(materialDropIntent('C:\\a').ok, false);
  const rejected = materialDropIntent(['C:\\packs\\cat', 'C:\\packs\\other']);
  assert.equal(String(rejected.message).includes('C:'), false, '固定提示不得回显路径');
});

test('拖放按实际物理坐标与 DPR 判断素材区域；区外/缺坐标不加载', () => {
  const rect = { left: 10, top: 20, width: 100, height: 50 };
  // CSS 矩形 (10,20)-(110,70)，DPR=1 时物理范围相同。
  assert.equal(materialDropWithinArea({ x: 10, y: 20 }, rect, 1), true);
  assert.equal(materialDropWithinArea({ x: 110, y: 70 }, rect, 1), true);
  assert.equal(materialDropWithinArea({ x: 9, y: 20 }, rect, 1), false);
  assert.equal(materialDropWithinArea({ x: 10, y: 71 }, rect, 1), false);
  // DPR=4：物理范围 (40,80)-(440,280)，边界含端点。
  assert.equal(materialDropWithinArea({ x: 40, y: 80 }, rect, 4), true);
  assert.equal(materialDropWithinArea({ x: 440, y: 280 }, rect, 4), true);
  assert.equal(materialDropWithinArea({ x: 39, y: 80 }, rect, 4), false);
  assert.equal(materialDropWithinArea({ x: 441, y: 80 }, rect, 4), false);
  // 小于 1 的 DPR 也按真实比例映射，不能一律当成 1 而放宽区域。
  assert.equal(materialDropWithinArea({ x: 80, y: 30 }, rect, 1), true);
  assert.equal(materialDropWithinArea({ x: 80, y: 30 }, rect, 0.5), false);
  // 非法/缺失输入一律区域外，避免“猜一个默认区域”而误加载。
  assert.equal(materialDropWithinArea(null, rect, 4), false);
  assert.equal(materialDropWithinArea({ x: 100, y: 100 }, null, 4), false);
  assert.equal(materialDropWithinArea({ x: Number.NaN, y: 100 }, rect, 4), false);
  assert.equal(materialDropWithinArea({ x: 100, y: 100 }, { left: 0, top: 0, width: 0, height: 10 }, 1), false);
  // 非法 DPR 退化为 1，不静默放大接受范围。
  assert.equal(materialDropWithinArea({ x: 111, y: 21 }, rect, Number.NaN), false);
  assert.equal(materialDropWithinArea({ x: 110, y: 70 }, rect, 0), true);
});

test('单包解码预算常量与共享投影区分：面板展示 32MiB，投影上限仍为 64MiB', () => {
  assert.equal(MATERIAL_PACK_PIXEL_BUDGET_BYTES, 32 * 1024 * 1024);
  assert.equal(MATERIAL_LIMITS.decodedMaxBytes, 64 * 1024 * 1024);
});

test('素材命令 envelope 只携带 command 与完整 B context，不携带 buddyCommit/管理稿字段', () => {
  const state = packageState();
  const contextBefore = context();
  const envelope = materialCommandEnvelope(playMaterialCommand('idle', state), contextBefore);
  assert.deepEqual(Object.keys(envelope).sort(), ['command', 'context']);
  assert.deepEqual(Object.keys(envelope.context).sort(), ['expectedBuddiesRevision', 'expectedBuddyId']);
  assert.deepEqual(envelope.context, context());
  assert.notEqual(envelope.context, contextBefore);
  const serialized = JSON.stringify(envelope);
  assert.equal(serialized.includes('buddyCommit'), false);
  assert.equal(serialized.includes('management'), false);
  assert.equal(serialized.includes('path'), false);
});

test('素材状态/帧/命令不消费或改写 B 的回执、管理稿与 Unknown 状态', () => {
  const snapshot = Object.freeze({
    activeBuddyId: 'buddy-a',
    buddiesRevision: 7,
    buddyCommit: Object.freeze({
      kind: 'configure_model',
      fromBuddyId: 'buddy-a',
      fromRevision: 3,
      toBuddyId: 'buddy-a',
      toRevision: 4,
      managementVersion: 5,
    }),
    managementDraft: Object.freeze({ version: 5, dirty: true, kind: 'general' }),
    managementExit: Object.freeze({ version: 5, intent: 'close_chat' }),
  });
  const before = JSON.stringify(snapshot);
  const state = packageState();
  parseMaterialFrame(packageFrame());
  materialCommandEnvelope(loadMaterialCommand('C:\\packs\\cat', state), {
    expectedBuddyId: snapshot.activeBuddyId,
    expectedBuddiesRevision: snapshot.buddiesRevision,
  });
  materialCommandEnvelope(playMaterialCommand('idle', state), {
    expectedBuddyId: snapshot.activeBuddyId,
    expectedBuddiesRevision: snapshot.buddiesRevision,
  });
  assert.equal(JSON.stringify(snapshot), before);
  assert.equal(snapshot.buddyCommit !== null, true);
  assert.equal(snapshot.managementDraft.dirty, true);
  assert.equal(snapshot.managementExit.intent, 'close_chat');
});

test('素材状态按 revision 严格单调，旧查询/事件不得覆盖新状态', () => {
  const ordered = new RevisionOrderedMaterialStates();
  assert.equal(ordered.apply(builtinState({ revision: 1 })), true);
  assert.equal(ordered.apply(builtinState({ revision: 3 })), true);
  assert.equal(ordered.apply(builtinState({ revision: 2 })), false);
  assert.equal(ordered.apply(builtinState({ revision: 3 })), false);
  assert.equal(ordered.current.revision, 3);
  ordered.reset();
  assert.equal(ordered.current, null);
});

test('共享夹具冻结素材字段、枚举、命令与预算，解析器不保留缺字段旧协议', () => {
  assert.deepEqual(IPC_CONTRACT.materialStateFields, [
    'revision',
    'resourceGeneration',
    'playbackGeneration',
    'source',
    'pack',
    'actions',
    'actionId',
    'status',
    'loading',
    'decodedBytes',
    'frameCount',
    'error',
  ]);
  assert.deepEqual(IPC_CONTRACT.frameFields, [
    'resourceGeneration',
    'playbackGeneration',
    'presentationSequence',
    'source',
    'clipId',
    'frameIndex',
    'width',
    'height',
    'paused',
    'faulted',
  ]);
  assert.deepEqual(IPC_CONTRACT.materialSources, MATERIAL_SOURCES);
  assert.deepEqual(IPC_CONTRACT.materialStatuses, MATERIAL_STATUSES);
  assert.deepEqual(IPC_CONTRACT.materialCommandTypes, MATERIAL_COMMAND_TYPES);
  assert.deepEqual(IPC_CONTRACT.materialReplyFields, ['ok', 'state', 'error']);
  assert.deepEqual(IPC_CONTRACT.materialFrameTripleFields, [
    'resourceGeneration',
    'playbackGeneration',
    'presentationSequence',
  ]);
  assert.deepEqual(IPC_CONTRACT.materialLimits, MATERIAL_LIMITS);
  assert.equal(MATERIAL_LIMITS.frameMaxDimension, 512);
  assert.equal(MATERIAL_LIMITS.binaryMaxBytes, 1024 * 1024);
  assert.equal(MATERIAL_LIMITS.presentationBudgetBytes, 16 * 1024 * 1024);
  assert.equal(MATERIAL_LIMITS.displayMaxDimension, 1024);
  assert.equal(MATERIAL_LIMITS.maskMaxDimension, 256);
});

test('素材 ID 与显示名边界：ASCII 安全 ID、Unicode 标量与拒绝控制字符', () => {
  assert.equal(isMaterialId('idle-four-frame'), true);
  assert.equal(isMaterialId('a'), true);
  assert.equal(isMaterialId('a'.repeat(64)), true);
  assert.equal(isMaterialId('a'.repeat(65)), false);
  assert.equal(isMaterialId('bad id'), false);
  assert.equal(isMaterialId('动作'), false);
  assert.equal(isMaterialLabel('待机'), true);
  assert.equal(isMaterialLabel('😺'), true);
  assert.equal(isMaterialLabel('a'.repeat(65)), false);
  assert.equal(isMaterialLabel('a\nb'), false);
  assert.equal(isMaterialLabel(''), false);
});
