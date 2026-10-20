const test = require('node:test');
const assert = require('node:assert/strict');

const { MaskSyncQueue } = require('../.test-build/maskSync.js');

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

function mask(id) {
  return { width: id, height: 1, alpha: [id] };
}

function controlledDispatch() {
  const calls = [];
  const results = [];
  const dispatch = (value) =>
    new Promise((resolve, reject) => {
      calls.push({ value, resolve, reject });
    });
  const queue = new MaskSyncQueue({
    dispatch,
    onSettled: (value, result) => results.push({ value, result }),
  });
  return { calls, results, queue };
}

test('连续 DPI/尺寸变化只保留一个在途和一个最新掩码', async () => {
  const { calls, results, queue } = controlledDispatch();
  queue.enqueue(mask(0));
  for (let index = 1; index <= 100; index += 1) queue.enqueue(mask(index));

  assert.equal(calls.length, 1);
  assert.equal(calls[0].value.width, 0);
  assert.equal(queue.pendingMask.width, 100);

  calls[0].resolve();
  await tick();
  assert.equal(calls.length, 2);
  assert.equal(calls[1].value.width, 100);
  assert.equal(queue.pendingMask, null);
  assert.equal(results[0].result.superseded, true, '旧掩码结果不得清除最新状态');

  calls[1].resolve();
  await tick();
  assert.equal(queue.hasPendingWork, false);
  assert.equal(results[1].result.ok, true);
  assert.equal(results[1].result.superseded, false);
});

test('卸载取消后不再安装尚未发出的网格', async () => {
  const { calls, results, queue } = controlledDispatch();
  queue.enqueue(mask(1));
  queue.enqueue(mask(2));
  queue.cancel();
  assert.equal(queue.pendingMask, null);

  calls[0].resolve();
  await tick();
  assert.equal(calls.length, 1, '取消后不得继续派发待装掩码');
  assert.equal(results.length, 0, '取消后的结果不再回报界面');

  queue.enqueue(mask(3));
  await tick();
  assert.equal(calls.length, 1);
});

test('安装失败保留最新错误，队列仍可继续安装后续网格', async () => {
  const { calls, results, queue } = controlledDispatch();
  queue.enqueue(mask(1));
  queue.enqueue(mask(2));
  calls[0].reject(new Error('host rejected'));
  await tick();

  assert.equal(results.length, 1);
  assert.equal(results[0].result.ok, false);
  assert.equal(results[0].result.superseded, true);
  assert.equal(calls.length, 2);
  assert.equal(calls[1].value.width, 2);

  calls[1].reject(new Error('host rejected latest'));
  await tick();
  assert.equal(results[1].result.ok, false);
  assert.equal(results[1].result.superseded, false);
});
