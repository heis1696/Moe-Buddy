const test = require('node:test');
const assert = require('node:assert/strict');

const {
  LISTENER_RELEASE_ERROR,
  releaseListener,
  releaseListeners,
} = require('../.test-build/listeners.js');

const RAW_SECRET = 'SECRET C:\\Users\\someone\\token=abc';

test('同步抛错的 unlisten 收敛为固定安全错误，不泄露原始正文', async () => {
  const outcome = await releaseListener(() => {
    throw new Error(RAW_SECRET);
  });
  assert.equal(outcome.ok, false);
  assert.equal(outcome.error.code, LISTENER_RELEASE_ERROR.code);
  assert.equal(outcome.error.message, LISTENER_RELEASE_ERROR.message);
  assert.ok(!outcome.error.message.includes('SECRET'));
  assert.ok(!JSON.stringify(outcome).includes(RAW_SECRET));
});

test('异步拒绝的 unlisten 同样收敛为固定安全错误', async () => {
  const outcome = await releaseListener(async () => {
    throw new Error(RAW_SECRET);
  });
  assert.equal(outcome.ok, false);
  assert.equal(outcome.error.message, LISTENER_RELEASE_ERROR.message);
  assert.ok(!JSON.stringify(outcome).includes(RAW_SECRET));
});

test('成功释放返回 ok，并且确实调用了宿主授予的 unlisten', async () => {
  let called = 0;
  const outcome = await releaseListener(() => {
    called += 1;
  });
  assert.deepEqual(outcome, { ok: true });
  assert.equal(called, 1);
});

test('多个监听逐个释放；单个失败不阻止其余释放，失败结果可见', async () => {
  const released = [];
  const outcome = await releaseListeners([
    () => {
      released.push('first');
      throw new Error(RAW_SECRET);
    },
    () => {
      released.push('second');
    },
    async () => {
      released.push('third');
      throw new Error(RAW_SECRET);
    },
  ]);
  assert.deepEqual(released, ['first', 'second', 'third']);
  assert.equal(outcome.ok, false);
  assert.equal(outcome.error.code, 'listener_release_failed');
  assert.ok(!JSON.stringify(outcome).includes(RAW_SECRET));
});

test('全部成功时释放结果为 ok', async () => {
  const released = [];
  const outcome = await releaseListeners([
    () => released.push('a'),
    async () => {
      released.push('b');
    },
  ]);
  assert.deepEqual(outcome, { ok: true });
  assert.deepEqual(released, ['a', 'b']);
});
