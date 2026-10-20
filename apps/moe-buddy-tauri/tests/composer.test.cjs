const test = require('node:test');
const assert = require('node:assert/strict');

const { resolveComposerKey } = require('../.test-build/composer.js');

function enter(overrides = {}) {
  return {
    key: 'Enter',
    shiftKey: false,
    composing: false,
    eventComposing: false,
    keyCode: 13,
    canSend: true,
    ...overrides,
  };
}

test('IME 组合期间 Enter 不发送，即使草稿可发送', () => {
  assert.equal(resolveComposerKey(enter({ composing: true })), 'ignore');
  assert.equal(resolveComposerKey(enter({ eventComposing: true })), 'ignore');
  assert.equal(resolveComposerKey(enter({ keyCode: 229 })), 'ignore');
});

test('组合输入期间的 Shift+Enter 也不发送', () => {
  assert.equal(resolveComposerKey(enter({ composing: true, shiftKey: true })), 'ignore');
});

test('组合结束后 Enter 发送，Shift+Enter 换行', () => {
  assert.equal(resolveComposerKey(enter()), 'send');
  assert.equal(resolveComposerKey(enter({ shiftKey: true })), 'newline');
});

test('非 Enter 键不拦截，空/禁用草稿的 Enter 不发送', () => {
  assert.equal(resolveComposerKey(enter({ key: 'a' })), 'ignore');
  assert.equal(resolveComposerKey(enter({ canSend: false })), 'ignore');
});
