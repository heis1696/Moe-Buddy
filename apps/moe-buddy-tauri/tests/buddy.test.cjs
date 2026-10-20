const test = require('node:test');
const assert = require('node:assert/strict');

const {
  BUDDY_LABEL_MAX,
  BUDDY_PERSONA_MAX_CHARS,
  BUDDY_REGISTRY_MAX,
  BuddyDraftDiscardWatermarks,
  BuddySessionDrafts,
  activeBuddyOf,
  buddyCatalogOf,
  buddyExpectationOf,
  buddyLabelProblem,
  buddyPersonaProblem,
  buddyScopeChanged,
  isPositiveRevision,
  isSafeBuddyId,
  isSafeBuddyLabel,
  parseBuddyCatalog,
} = require('../.test-build/buddy.js');
const { IPC_CONTRACT } = require('../.test-build/contract.js');
const { parseSnapshot } = require('../.test-build/snapshot.js');
const {
  displayActiveBuddy,
  displayBuddyCatalog,
  displayBuddies,
  buddyLabel,
  buddyRevisionText,
} = require('../.test-build/projection.js');
const { parseSessionCatalog } = require('../.test-build/session.js');

function buddy(id, overrides = {}) {
  return {
    id,
    label: `伙伴 ${id}`,
    profileRevision: 1,
    personaRevision: 1,
    modelRevision: 1,
    ...overrides,
  };
}

function catalog(overrides = {}) {
  return {
    activeBuddyId: 'buddy-a',
    buddiesRevision: 3,
    buddies: [buddy('buddy-a'), buddy('buddy-b')],
    ...overrides,
  };
}

test('伙伴 ID 与 Rust 对齐：首字符字母/数字，其余仅字母数字点下划连字符；显示名按 Unicode 标量', () => {
  assert.equal(isSafeBuddyId('buddy-a'), true);
  assert.equal(isSafeBuddyId('A.b_c-1'), true);
  assert.equal(isSafeBuddyId('a'.repeat(64)), true);
  assert.equal(isSafeBuddyId('a'.repeat(65)), false);
  assert.equal(isSafeBuddyId(''), false);
  assert.equal(isSafeBuddyId('has space'), false);
  assert.equal(isSafeBuddyId('bad\tid'), false);
  assert.equal(isSafeBuddyId('伙伴'), false);
  assert.equal(isSafeBuddyId('-leading'), false, '首字符必须是 ASCII 字母或数字');
  assert.equal(isSafeBuddyId('.hidden'), false);
  assert.equal(isSafeBuddyId('.'), false);
  assert.equal(isSafeBuddyId('..'), false);
  assert.equal(isSafeBuddyId('a/b'), false);
  assert.equal(isSafeBuddyId('a:b'), false);
  assert.equal(isSafeBuddyId(7), false);
  assert.equal(isSafeBuddyId(null), false);

  assert.equal(isSafeBuddyLabel('苔'), true);
  assert.equal(isSafeBuddyLabel(''), true, '空显示名由界面回退到 ID');
  assert.equal(isSafeBuddyLabel('a'.repeat(64)), true);
  assert.equal(isSafeBuddyLabel('a'.repeat(65)), false);
  assert.equal(isSafeBuddyLabel('😀'.repeat(64)), true, '按 Unicode 标量而非 UTF-16 单元计数');
  assert.equal(isSafeBuddyLabel('😀'.repeat(65)), false);
  assert.equal(isSafeBuddyLabel('bad\u0007name'), false);
  assert.equal(isSafeBuddyLabel('bad\u007fname'), false);
  assert.equal(isSafeBuddyLabel(7), false);
  assert.equal(BUDDY_LABEL_MAX, 64);
  assert.equal(BUDDY_REGISTRY_MAX, 8);

  assert.equal(buddyLabelProblem('   '), '伙伴名称不能为空。');
  assert.match(buddyLabelProblem('a'.repeat(65)), /不能超过 64/);
  assert.equal(buddyLabelProblem('正常名称'), null);
  assert.equal(buddyPersonaProblem('多行\n人设\r含回车\t含制表'), null);
  assert.equal(buddyPersonaProblem('😀'.repeat(4096)), null, '人设按 Unicode 标量计数');
  assert.match(buddyPersonaProblem('😀'.repeat(4097)), /不能超过 4096/);
  assert.match(buddyPersonaProblem('   '), /不能为空/);
  assert.match(buddyPersonaProblem('坏\u0000人设'), /控制字符/);
});

test('修订必须是正的 JavaScript 安全整数；最大安全整数之后明确拒绝', () => {
  assert.equal(isPositiveRevision(1), true);
  assert.equal(isPositiveRevision(Number.MAX_SAFE_INTEGER), true);
  assert.equal(isPositiveRevision(0), false);
  assert.equal(isPositiveRevision(-1), false);
  assert.equal(isPositiveRevision(1.5), false);
  assert.equal(isPositiveRevision(Number.MAX_SAFE_INTEGER + 1), false);
  assert.equal(isPositiveRevision('3'), false);
});

test('伙伴注册表三元组必填，缺任一字段整份拒绝而不是兼容旧快照', () => {
  assert.equal(parseBuddyCatalog({}, false), null);
  for (const field of ['activeBuddyId', 'buddiesRevision', 'buddies']) {
    const partial = { ...catalog() };
    delete partial[field];
    assert.equal(parseBuddyCatalog(partial, false), null, field);
  }
  assert.equal(buddyCatalogOf(null), null);
  assert.equal(buddyCatalogOf({}), null);
  assert.deepEqual(buddyExpectationOf(null), {
    expectedBuddyId: '',
    expectedBuddiesRevision: 0,
  });
});

test('伙伴注册表严格校验字段、上限、唯一 ID 与活动选择', () => {
  assert.deepEqual(parseBuddyCatalog(catalog(), false), catalog());
  assert.equal(parseBuddyCatalog(catalog({ activeBuddyId: 'missing' }), false), null);
  assert.equal(
    parseBuddyCatalog(catalog({ buddies: [...catalog().buddies, buddy('buddy-a')] }), false),
    null,
  );
  assert.equal(
    parseBuddyCatalog(
      catalog({ buddies: Array.from({ length: 9 }, (_, index) => buddy(`b-${index}`)), activeBuddyId: 'b-0' }),
      false,
    ),
    null,
  );
  assert.equal(
    parseBuddyCatalog(catalog({ buddies: [{ ...buddy('buddy-a'), extra: true }] }), false),
    null,
  );
  assert.equal(
    parseBuddyCatalog(catalog({ buddies: [{ ...buddy('buddy-a'), modelRevision: 0 }] }), false),
    null,
  );
  assert.equal(parseBuddyCatalog(catalog({ buddiesRevision: 0 }), false), null);
  assert.equal(
    parseBuddyCatalog(catalog({ buddiesRevision: Number.MAX_SAFE_INTEGER + 1 }), false),
    null,
  );
  assert.equal(parseBuddyCatalog(catalog({ activeBuddyId: null }), false), null, '有档案但无选择是悬空状态');
  assert.deepEqual(
    parseBuddyCatalog(catalog({ activeBuddyId: null, buddies: [] }), false),
    { activeBuddyId: null, buddiesRevision: 3, buddies: [] },
  );
});

test('撤权后活动伙伴为 null 且注册表必须为空', () => {
  assert.deepEqual(parseBuddyCatalog(catalog({ activeBuddyId: null, buddies: [] }), true), {
    activeBuddyId: null,
    buddiesRevision: 3,
    buddies: [],
  });
  assert.equal(parseBuddyCatalog(catalog(), true), null);
  assert.equal(parseBuddyCatalog(catalog({ activeBuddyId: null }), true), null);
});

test('严格快照要求伙伴三元组；缺字段、多余字段或矛盾状态整份拒绝', () => {
  const base = {
    revision: 1,
    presentationRevision: 1,
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
    activeSessionId: null,
    sessionsRevision: 1,
    sessions: [],
    sessionIsolationAllowed: false,
    activeBuddyPersona: '伙伴 A 的合成人设',
    buddyCommit: null,
    ...catalog(),
  };
  assert.notEqual(parseSnapshot(base), null);
  for (const field of ['activeBuddyId', 'buddiesRevision', 'buddies']) {
    const partial = { ...base };
    delete partial[field];
    assert.equal(parseSnapshot(partial), null, `missing ${field}`);
  }
  assert.equal(parseSnapshot({ ...base, unknownBuddyField: 1 }), null);
  assert.equal(parseSnapshot({ ...base, activeBuddyId: null }), null);
  assert.equal(
    parseSnapshot({ ...base, buddies: [{ ...buddy('buddy-a'), personaRevision: 0 }] }),
    null,
  );
  // 会话目录严格性不受伙伴字段影响：缺 A 的会话许可仍整份拒绝。
  const withoutIsolation = { ...base };
  delete withoutIsolation.sessionIsolationAllowed;
  assert.equal(parseSnapshot(withoutIsolation), null);
});

test('伙伴投影与身份版本只读展示；撤权后不渲染旧注册表', () => {
  const snapshot = {
    revoked: false,
    ...catalog(),
  };
  assert.deepEqual(displayBuddyCatalog(snapshot), catalog());
  assert.deepEqual(displayBuddies(snapshot), catalog().buddies);
  assert.equal(displayActiveBuddy(snapshot).id, 'buddy-a');
  assert.equal(buddyLabel({ ...buddy('buddy-a'), label: '  ' }), 'buddy-a');
  assert.equal(
    buddyRevisionText(catalog().buddies[0]),
    '档案 r1 · 人设 r1 · 模型 r1',
  );
  assert.equal(displayActiveBuddy({ revoked: true, ...catalog() }), null);
  assert.deepEqual(displayBuddies({ revoked: true, ...catalog() }), []);
  assert.equal(buddyCatalogOf({ activeBuddyId: null, buddiesRevision: 1, buddies: [] }).activeBuddyId, null);
  assert.equal(activeBuddyOf({ activeBuddyId: null, buddiesRevision: 1, buddies: [] }), null);
  assert.equal(
    buddyScopeChanged(
      { expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 1 },
      { expectedBuddyId: 'buddy-b', expectedBuddiesRevision: 2 },
    ),
    true,
  );
  assert.equal(
    buddyScopeChanged(
      { expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 1 },
      { expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 2 },
    ),
    true,
    '同 ID 的选择修订变化也必须视为作用域栅栏变化',
  );
});

test('同一局部会话 ID 在不同伙伴下互不共享草稿；伙伴槽位有界', () => {
  const drafts = new BuddySessionDrafts(2, 2);
  drafts.remember('buddy-a', 's-a', 'A 的草稿', 1, 0, true);
  drafts.remember('buddy-b', 's-a', 'B 的草稿', 2, 0, true);
  assert.equal(drafts.recallEntry('buddy-a', 's-a').text, 'A 的草稿');
  assert.equal(drafts.recallEntry('buddy-b', 's-a').text, 'B 的草稿');
  drafts.markSynced('buddy-a', 's-a', 'A 的草稿', 4);
  assert.equal(drafts.peek('buddy-b', 's-a').revision, 2, '同步修订不跨伙伴');

  // 每个伙伴内部按会话有界淘汰。
  drafts.remember('buddy-a', 's-b', 'b1');
  drafts.remember('buddy-a', 's-c', 'c1');
  assert.equal(drafts.count('buddy-a'), 2);
  assert.equal(drafts.peek('buddy-a', 's-a'), null);

  // 伙伴作用域总数有界，最久未用的作用域被淘汰（上面的同步刷新了 buddy-a）。
  drafts.remember('buddy-c', 's-a', 'C 的草稿');
  assert.equal(drafts.scopeCount, 2);
  assert.equal(drafts.count('buddy-b'), 0);
  assert.equal(drafts.count('buddy-a'), 2);
  assert.equal(drafts.peek('buddy-c', 's-a').text, 'C 的草稿');

  drafts.prune(['buddy-b']);
  assert.equal(drafts.scopeCount, 0, '不在当前注册表的作用域被裁剪');
  drafts.remember('buddy-b', 's-b', 'B 的草稿');
  assert.equal(drafts.count('buddy-b'), 1);
  // 空串只是“尚无伙伴”的上下文哨兵，不能成为草稿业务作用域。
  drafts.remember('', 's-b', '哨兵草稿');
  assert.equal(drafts.count(''), 0);
  drafts.clear();
  assert.equal(drafts.scopeCount, 0);
});

test('聊天草稿丢弃水位按伙伴单调隔离，其他伙伴编辑不让旧回执失效', () => {
  const watermarks = new BuddyDraftDiscardWatermarks(2, 2);
  assert.equal(watermarks.get('buddy-a', 's-a'), 0);
  assert.equal(watermarks.raise('buddy-a', 's-a', 3), 3);
  assert.equal(watermarks.raise('buddy-a', 's-a', 2), 3, '水位不倒退');
  assert.equal(watermarks.get('buddy-b', 's-a'), 0, '同局部会话 ID 的水位互不共享');
  assert.equal(watermarks.raise('buddy-b', 's-a', 5), 5);
  watermarks.prune(['buddy-a']);
  assert.equal(watermarks.get('buddy-b', 's-a'), 0);
  watermarks.clear();
  assert.equal(watermarks.get('buddy-a', 's-a'), 0);
});

test('共享夹具包含 B 契约字段、限值与命令，A 字段保持必填', () => {
  assert.deepEqual(IPC_CONTRACT.buddySnapshotFields, [
    'activeBuddyId',
    'buddiesRevision',
    'buddies',
  ]);
  assert.deepEqual(IPC_CONTRACT.buddyFields, [
    'id',
    'label',
    'profileRevision',
    'personaRevision',
    'modelRevision',
  ]);
  assert.deepEqual(IPC_CONTRACT.buddyLimits, {
    idMaxBytes: 64,
    registryMax: 8,
    labelMaxScalars: 64,
    personaMaxChars: 4096,
  });
  assert.deepEqual(IPC_CONTRACT.dispatchContextFields, [
    'expectedBuddyId',
    'expectedBuddiesRevision',
  ]);
  assert.deepEqual(IPC_CONTRACT.managementDraftStateFields, ['context', 'dirty', 'kind', 'version']);
  assert.deepEqual(IPC_CONTRACT.managementDraftKinds, [
    'none',
    'general',
    'buddy_create',
    'buddy_rename',
  ]);
  assert.deepEqual(IPC_CONTRACT.buddyCommands, [
    'create_buddy',
    'switch_buddy',
    'rename_buddy',
  ]);
  for (const command of IPC_CONTRACT.buddyCommands) {
    assert.ok(IPC_CONTRACT.dispatchCommands.includes(command), command);
  }
  assert.deepEqual(IPC_CONTRACT.buddyCreateFields, [
    'label',
    'persona',
    'expectedManagementDraftVersion',
  ]);
  assert.deepEqual(IPC_CONTRACT.buddySwitchFields, ['id']);
  assert.deepEqual(IPC_CONTRACT.buddyRenameFields, [
    'label',
    'expectedManagementDraftVersion',
  ]);
  assert.ok(IPC_CONTRACT.snapshotFields.includes('activeBuddyPersona'));
  // 冻结夹具仍要求 A 会话三元组必填，B 只做加法而不是替代。
  assert.deepEqual(parseSessionCatalog(catalog(), false), null);
});
