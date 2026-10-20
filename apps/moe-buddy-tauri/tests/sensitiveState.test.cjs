const test = require('node:test');
const assert = require('node:assert/strict');

const {
  clearedSensitiveDrafts,
  hasSensitiveDrafts,
} = require('../.test-build/sensitiveState.js');

test('撤权清理返回空的面板草稿缓存，不保留 API Key 或编辑正文', () => {
  const cleared = clearedSensitiveDrafts();
  assert.deepEqual(cleared, {
    apiKeyInput: '',
    clearApiKey: false,
    memoryEdit: null,
    rememberDraft: '',
    searchQuery: '',
  });
  assert.equal(hasSensitiveDrafts(cleared), false);
});

test('敏感草稿检测覆盖密码、清除勾选、编辑、新增与检索词', () => {
  assert.equal(hasSensitiveDrafts({ ...clearedSensitiveDrafts(), apiKeyInput: 'secret' }), true);
  assert.equal(hasSensitiveDrafts({ ...clearedSensitiveDrafts(), clearApiKey: true }), true);
  assert.equal(
    hasSensitiveDrafts({
      ...clearedSensitiveDrafts(),
      memoryEdit: { id: '1', revision: 1, baseText: 'old' },
    }),
    true,
  );
  assert.equal(hasSensitiveDrafts({ ...clearedSensitiveDrafts(), rememberDraft: '新记忆' }), true);
  assert.equal(hasSensitiveDrafts({ ...clearedSensitiveDrafts(), searchQuery: 'tea' }), true);
  assert.equal(hasSensitiveDrafts({ ...clearedSensitiveDrafts(), searchQuery: '   ' }), false);
});
