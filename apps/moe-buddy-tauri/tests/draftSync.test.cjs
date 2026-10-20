const test = require('node:test');
const assert = require('node:assert/strict');

const { DraftSyncQueue } = require('../.test-build/draftSync.js');

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

function makeSnapshot(revision) {
  return {
    revision,
    mode: 'offline',
    revoked: false,
    closeConfirmation: false,
    chat: { draft: '', messages: [], status: 'ready', failure: null },
  };
}

function envelope(revision = 1) {
  return { snapshot: makeSnapshot(revision), error: null };
}

function controlledDispatch() {
  const calls = [];
  const dispatch = (text) =>
    new Promise((resolve, reject) => {
      calls.push({ text, resolve, reject });
    });
  return { calls, dispatch };
}

test('高频输入有界合并：只保留一个在途和一个最新待发', async () => {
  const { calls, dispatch } = controlledDispatch();
  const queue = new DraftSyncQueue({ dispatch });

  queue.enqueue('v0');
  for (let index = 1; index <= 500; index += 1) queue.enqueue(`v${index}`);

  assert.equal(calls.length, 1);
  assert.equal(calls[0].text, 'v0');
  assert.equal(queue.pendingText, 'v500');

  const settled = queue.flush();
  calls[0].resolve(envelope(1));
  await tick();

  assert.equal(calls.length, 2);
  assert.equal(calls[1].text, 'v500');
  assert.equal(queue.pendingText, null);

  calls[1].resolve(envelope(2));
  assert.equal(await settled, true);
  assert.equal(calls.length, 2);
  assert.equal(queue.latestRequested, 'v500');
  assert.equal(queue.isSynced(), true);
});

test('合并批次中的中间输入不逐条派发，最后一份文本生效', async () => {
  const { calls, dispatch } = controlledDispatch();
  const queue = new DraftSyncQueue({ dispatch });

  queue.enqueue('a');
  queue.enqueue('b');
  queue.enqueue('c');
  assert.equal(calls.length, 1);

  calls[0].resolve(envelope(1));
  await tick();
  assert.equal(calls.length, 2);
  assert.equal(calls[1].text, 'c');

  calls[1].resolve(envelope(2));
  assert.equal(await queue.flush(), true);
  assert.equal(queue.isSynced(), true);
});

test('撤权时丢弃尚未发出的草稿，之后仍可重新同步', async () => {
  const { calls, dispatch } = controlledDispatch();
  const queue = new DraftSyncQueue({ dispatch });

  queue.enqueue('already-sent');
  queue.enqueue('unsent');
  queue.discardPending();
  assert.equal(queue.pendingText, null);

  calls[0].resolve(envelope(1));
  await tick();
  assert.equal(calls.length, 1, '丢弃的草稿不得再派发');
  assert.equal(await queue.flush(), false);
  assert.equal(queue.isSynced(), false);

  const next = queue.flushLatest('after-revoke-failure');
  await tick();
  assert.equal(calls.length, 2);
  assert.equal(calls[1].text, 'after-revoke-failure');
  calls[1].resolve(envelope(2));
  assert.equal(await next, true);
});

test('卸载或已确认撤权后取消队列，不再接受新草稿', async () => {
  const { calls, dispatch } = controlledDispatch();
  const queue = new DraftSyncQueue({ dispatch });

  queue.enqueue('a');
  queue.enqueue('b');
  queue.cancel();

  calls[0].resolve(envelope(1));
  await tick();
  assert.equal(calls.length, 1);
  assert.equal(await queue.flush(), false);

  queue.enqueue('c');
  await tick();
  assert.equal(calls.length, 1);
  assert.equal(await queue.flushLatest('c'), false);
  assert.equal(queue.isSynced(), false);
});

test('失败后不自动重试同一草稿，显式 flush 才重试一次', async () => {
  const { calls, dispatch } = controlledDispatch();
  const queue = new DraftSyncQueue({ dispatch });

  queue.enqueue('draft');
  calls[0].resolve({
    snapshot: makeSnapshot(1),
    error: { code: 'host_timeout', message: 'host did not complete the command', disposition: 'unknown' },
  });

  assert.equal(await queue.flush(), false);
  await tick();
  assert.equal(calls.length, 1);

  const retried = queue.flushLatest('draft');
  await tick();
  assert.equal(calls.length, 2);
  assert.equal(calls[1].text, 'draft');
  calls[1].resolve(envelope(2));
  assert.equal(await retried, true);
});

test('发送清空宿主草稿后，重新输入相同文本仍会同步', async () => {
  const { calls, dispatch } = controlledDispatch();
  const queue = new DraftSyncQueue({ dispatch });

  const first = queue.flushLatest('hello');
  await tick();
  calls[0].resolve(envelope(1));
  assert.equal(await first, true);
  assert.equal(queue.isSynced(), true);

  queue.adoptHostDraft('');
  assert.equal(queue.isSynced(), true);

  queue.enqueue('hello');
  await tick();
  assert.equal(calls.length, 2);
  assert.equal(calls[1].text, 'hello');
  calls[1].resolve(envelope(2));
  assert.equal(await queue.flush(), true);
});

test('已在宿主确认的相同文本不重复派发', async () => {
  const { calls, dispatch } = controlledDispatch();
  const queue = new DraftSyncQueue({ dispatch });

  const first = queue.flushLatest('stable');
  await tick();
  calls[0].resolve(envelope(1));
  assert.equal(await first, true);

  queue.enqueue('stable');
  assert.equal(await queue.flushLatest('stable'), true);
  assert.equal(calls.length, 1);
});

test('A→B→A：回改到在途文本后丢弃过期的 B，不再补发', async () => {
  const { calls, dispatch } = controlledDispatch();
  const queue = new DraftSyncQueue({ dispatch });

  queue.enqueue('A');
  queue.enqueue('B');
  assert.equal(calls.length, 1);
  assert.equal(queue.pendingText, 'B');
  assert.equal(queue.latestRequested, 'B');

  queue.enqueue('A');
  assert.equal(queue.pendingText, null);
  assert.equal(queue.latestRequested, 'A');

  const settled = queue.flush();
  calls[0].resolve(envelope(1));
  assert.equal(await settled, true);
  await tick();
  assert.equal(calls.length, 1, '过期的 B 不得继续派发');
  assert.equal(calls[0].text, 'A');
  assert.equal(queue.isSynced(), true);
});

test('A→B→A 后 flushLatest(A) 只等待在途 A，不误放行 B', async () => {
  const { calls, dispatch } = controlledDispatch();
  const queue = new DraftSyncQueue({ dispatch });

  queue.enqueue('A');
  queue.enqueue('B');
  const settled = queue.flushLatest('A');
  assert.equal(calls.length, 1);
  assert.equal(queue.pendingText, null);

  calls[0].resolve(envelope(1));
  assert.equal(await settled, true);
  await tick();
  assert.equal(calls.length, 1);
  assert.equal(queue.latestRequested, 'A');
  assert.equal(queue.isSynced(), true);
});

test('旧在途请求失败且用户已回改：保持未同步并由显式 flushLatest 重试', async () => {
  const { calls, dispatch } = controlledDispatch();
  const queue = new DraftSyncQueue({ dispatch });

  queue.enqueue('A');
  queue.enqueue('B');
  queue.enqueue('A');
  const settled = queue.flushLatest('A');
  calls[0].resolve({
    snapshot: makeSnapshot(1),
    error: { code: 'host_timeout', message: 'host did not complete the command', disposition: 'unknown' },
  });

  assert.equal(await settled, false);
  await tick();
  assert.equal(calls.length, 1, '失败后不得自动补发或派发过期 B');
  assert.equal(queue.latestRequested, 'A');
  assert.equal(queue.pendingText, null);
  assert.equal(queue.isSynced(), false);

  const retried = queue.flushLatest('A');
  await tick();
  assert.equal(calls.length, 2);
  assert.equal(calls[1].text, 'A');
  calls[1].resolve(envelope(2));
  assert.equal(await retried, true);
  assert.equal(queue.latestRequested, 'A');
  assert.equal(queue.isSynced(), true);
});

test('回改后再次输入新文本，仍只保留一份最新待发', async () => {
  const { calls, dispatch } = controlledDispatch();
  const queue = new DraftSyncQueue({ dispatch });

  queue.enqueue('A');
  queue.enqueue('B');
  queue.enqueue('A');
  queue.enqueue('C');
  assert.equal(queue.pendingText, 'C');
  assert.equal(queue.latestRequested, 'C');

  const settled = queue.flush();
  calls[0].resolve(envelope(1));
  await tick();
  assert.equal(calls.length, 2);
  assert.equal(calls[1].text, 'C');

  calls[1].resolve(envelope(2));
  assert.equal(await settled, true);
  assert.equal(queue.isSynced(), true);
});
