const test = require('node:test');
const assert = require('node:assert/strict');

const { CommandUnconfirmedLock } = require('../.test-build/refresh.js');

test('未知副作用命令在刷新开始后仍保持锁定', () => {
  const lock = new CommandUnconfirmedLock();
  lock.markUnknown('send', 'business_unknown');
  lock.beginRefresh();
  assert.equal(lock.locked, true);
  assert.equal(lock.isRefreshing, true);
});

test('普通状态重读不能解除未知副作用锁', () => {
  const lock = new CommandUnconfirmedLock();
  lock.markUnknown('send', 'business_unknown');
  lock.beginRefresh();
  lock.finishRefresh(false, true);
  assert.equal(lock.locked, true, '读取失败时必须保持锁定');

  lock.beginRefresh();
  lock.finishRefresh(true, false);
  assert.equal(lock.locked, true, '空快照不能证明可以继续');

  lock.beginRefresh();
  lock.finishRefresh(true, true);
  assert.equal(lock.locked, true, '快照没有匹配命令回执，不能确认未知副作用已收口');
  assert.equal(lock.isRefreshing, false);
});

test('没有刷新上下文或普通快照事件不能解锁', () => {
  const lock = new CommandUnconfirmedLock();
  lock.markUnknown('send', 'ipc_reply_lost');
  lock.finishRefresh(true, true);
  assert.equal(lock.locked, true, '非刷新读取不得代替业务队列屏障');
});

test('命令自身的成功响应可确定结果并解锁', () => {
  const lock = new CommandUnconfirmedLock();
  lock.markUnknown('send', 'business_unknown');
  lock.confirm('send');
  assert.equal(lock.locked, false);
});

test('刷新后相同未知业务状态仍保持锁定，来源类型可区分 IPC 丢回包', () => {
  const lock = new CommandUnconfirmedLock();
  lock.markUnknown('send', 'ipc_reply_lost');
  assert.equal(lock.source, 'ipc_reply_lost');
  lock.beginRefresh();
  lock.finishRefresh(true, true);
  assert.equal(lock.locked, true);

  lock.markUnknown('send', 'business_unknown');
  assert.equal(lock.source, 'business_unknown');
  lock.beginRefresh();
  lock.finishRefresh(false, false);
  assert.equal(lock.locked, true);
});

test('发送 Unknown 按会话隔离：旧会话保持锁定，新会话没有会话未知', () => {
  const lock = new CommandUnconfirmedLock();
  lock.enterSession('session-a');
  lock.markUnknown('send', 'business_unknown');
  assert.equal(lock.locked, true);
  assert.equal(lock.hasSessionUnknown, true, 'send 未知只记录在发生它的会话');
  assert.equal(lock.hasGlobalUnknown, false);
  assert.equal(lock.operation, 'send');

  lock.enterSession('session-b');
  assert.equal(lock.locked, false, '新会话没有自己的发送未知');
  assert.equal(lock.hasSessionUnknown, false);

  lock.enterSession('session-a');
  assert.equal(lock.locked, true, '切回旧会话必须保留 Unknown');
  assert.equal(lock.source, 'business_unknown');
});

test('显式新建只写会话作用域，不自动重发也不解除旧 Unknown', () => {
  const lock = new CommandUnconfirmedLock();
  lock.enterSession('session-a');
  lock.markUnknown('send', 'ipc_reply_lost');

  // 新建成功只切换到新会话；旧会话条目仍在，普通刷新也不能清它。
  lock.enterSession('session-c');
  lock.beginRefresh();
  lock.finishRefresh(true, true);
  assert.equal(lock.locked, false);
  lock.enterSession('session-a');
  assert.equal(lock.locked, true);
  assert.equal(lock.source, 'ipc_reply_lost');

  // 只有匹配会话/操作的确认回执才能清对应条目。
  lock.confirm('send', 'session-c');
  assert.equal(lock.locked, true);
  lock.confirm('send', 'session-a');
  assert.equal(lock.locked, false);
});

test('记忆、设置、存储与撤权 Unknown 不能被新建会话或普通快照解除', () => {
  for (const operation of [
    'remember',
    'memory_update',
    'memory_delete',
    'model',
    'revoke',
    'close',
    'new_session',
  ]) {
    const lock = new CommandUnconfirmedLock();
    lock.enterSession('session-a');
    lock.markUnknown(operation, 'business_unknown');
    assert.equal(lock.locked, true, operation);
    assert.equal(lock.hasGlobalUnknown, true, operation);
    assert.equal(lock.hasSessionUnknown, false, operation);
    lock.enterSession('session-b');
    assert.equal(lock.locked, true, `${operation} 是全局未知，不能被新会话解除`);
    lock.beginRefresh();
    lock.finishRefresh(true, true);
    assert.equal(lock.locked, true, `${operation} 不能被普通快照解除`);
    assert.equal(lock.hasGlobalUnknown, true);
    lock.enterSession('session-a');
    assert.equal(lock.locked, true);
  }
});

test('发送成功回执只清发生命令的会话，迟到确认不清另一会话', () => {
  const lock = new CommandUnconfirmedLock();
  lock.enterSession('session-a');
  lock.markUnknown('send', 'ipc_reply_lost');
  lock.enterSession('session-b');
  lock.markUnknown('send', 'business_unknown');

  lock.confirm('send', 'session-a');
  assert.equal(lock.locked, true, 'B 仍在途未知');
  lock.confirm('send', 'session-b');
  assert.equal(lock.locked, false);
});

test('指定会话的成功确认不回退清全局未知', () => {
  const lock = new CommandUnconfirmedLock();
  lock.markUnknown('send', 'ipc_reply_lost', null);
  assert.equal(lock.hasGlobalUnknown, true);
  lock.confirm('send', 'session-a');
  assert.equal(lock.locked, true, '另一会话成功不能清全局发送未知');
  lock.confirm('send', null);
  assert.equal(lock.locked, false);
});

test('会话 ID 缺失的发送按全局未知处理，不产生会话作用域', () => {
  const lock = new CommandUnconfirmedLock();
  lock.markUnknown('send', 'ipc_reply_lost', null);
  assert.equal(lock.hasGlobalUnknown, true);
  assert.equal(lock.hasSessionUnknown, false);
  lock.enterSession('session-a');
  assert.equal(lock.locked, true);
});
