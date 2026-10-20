const test = require('node:test');
const assert = require('node:assert/strict');

const {
  armMemoryDelete,
  beginMemoryEdit,
  confirmMemoryDelete,
  isMemoryDeleteArmed,
  isMemoryRevisionConflict,
  memoryDeleteFailureText,
  memoryEditDirty,
  memoryEditSuperseded,
  memorySaveFailureText,
  retainMemoryDraft,
} = require('../.test-build/memoryEditor.js');

const memory = { id: '7', revision: 3, text: '喜欢 tea' };

test('版本化编辑带回原 revision，草稿变化才提示未保存', () => {
  const edit = beginMemoryEdit(memory);
  assert.deepEqual(edit, { id: '7', revision: 3, baseText: '喜欢 tea' });
  assert.equal(memoryEditDirty(edit, '喜欢 tea'), false);
  assert.equal(memoryEditDirty(edit, '喜欢 coffee'), true);
  assert.equal(memoryEditDirty(null, 'anything'), false);
});

test('快照里版本推进或条目消失时提示保存会发生冲突', () => {
  const edit = beginMemoryEdit(memory);
  assert.equal(memoryEditSuperseded(edit, [memory]), false);
  assert.equal(memoryEditSuperseded(edit, [{ ...memory, revision: 4 }]), true);
  assert.equal(memoryEditSuperseded(edit, []), true);
  assert.equal(memoryEditSuperseded(edit, [{ ...memory, text: '喜欢 coffee' }]), false);
  assert.equal(memoryEditSuperseded(null, []), false);
});

test('确认删除精确绑定用户看到的 id 与 revision', () => {
  const arm = armMemoryDelete(memory);
  assert.deepEqual(arm, { id: '7', revision: 3 });
  assert.equal(isMemoryDeleteArmed(arm, memory), true);
  assert.equal(isMemoryDeleteArmed(arm, { ...memory, revision: 4 }), false);
  assert.equal(isMemoryDeleteArmed(arm, { ...memory, id: '8' }), false);
  assert.equal(isMemoryDeleteArmed(null, memory), false);

  assert.equal(confirmMemoryDelete(arm, '7', 3), true);
  assert.equal(confirmMemoryDelete(arm, '7', 4), false);
  assert.equal(confirmMemoryDelete(arm, '8', 3), false);
  assert.equal(confirmMemoryDelete(null, '7', 3), false);
});

test('版本冲突的错误码被识别并提示保留草稿、先刷新', () => {
  for (const code of [
    'memory_revision_conflict',
  ]) {
    assert.equal(
      isMemoryRevisionConflict({ code, message: 'x', disposition: 'not_executed' }),
      true,
      code,
    );
    const text = memorySaveFailureText({ code, message: 'x', disposition: 'not_executed' });
    assert.match(text, /草稿已保留/);
    assert.match(text, /刷新列表/);
    assert.doesNotMatch(text, /自动重试/);
  }
});

test('普通失败保留编辑草稿语义，未知处置不诱导重复操作', () => {
  const text = memorySaveFailureText({
    code: 'host_timeout',
    message: 'host did not complete the command',
    disposition: 'unknown',
  });
  assert.match(text, /未确认/);
  assert.match(text, /不要重复操作/);
  assert.ok(!text.includes('草稿已删除'));
});

test('删除失败的提示区分版本冲突与普通失败', () => {
  assert.match(
    memoryDeleteFailureText({
      code: 'memory_revision_conflict',
      message: 'x',
      disposition: 'not_executed',
    }),
    /删除未执行/,
  );
  assert.match(
    memoryDeleteFailureText({
      code: 'host_error',
      message: 'host said no',
      disposition: 'settled',
    }),
    /已确认/,
  );
});

test('保存失败原样保留编辑目标与草稿，不推进 revision', () => {
  const edit = beginMemoryEdit(memory);
  const draft = '喜欢 coffee（未保存）';
  const conflict = retainMemoryDraft(edit, draft, {
    code: 'memory_revision_conflict',
    message: 'x',
    disposition: 'not_executed',
  });
  assert.deepEqual(conflict.edit, edit);
  assert.equal(conflict.draft, draft);
  assert.equal(conflict.conflict, true);
  assert.match(conflict.message, /草稿已保留/);

  const generic = retainMemoryDraft(edit, draft, {
    code: 'host_error',
    message: 'host said no',
    disposition: 'settled',
  });
  assert.deepEqual(generic.edit, edit);
  assert.equal(generic.draft, draft);
  assert.equal(generic.conflict, false);
  assert.equal(generic.edit.revision, 3);
});

test('arbitrary conflict-like codes are not classified as definite memory conflicts', () => {
  for (const code of ['revision_conflict', 'stale_revision', 'host_conflict', 'memory_not_found']) {
    assert.equal(
      isMemoryRevisionConflict({ code, message: 'x', disposition: 'not_executed' }),
      false,
      code,
    );
  }
});
