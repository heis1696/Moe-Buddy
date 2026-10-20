const test = require('node:test');
const assert = require('node:assert/strict');

const { chatStatusAllowsSend } = require('../.test-build/chatStatus.js');
const {
  PrototypeBoundaryError,
  commandDefinitelyNotExecuted,
  describeHostFailure,
  describeThrownFailure,
  formatSafeError,
  isSideEffectOperation,
} = require('../.test-build/failure.js');

const RAW_SECRET = 'SECRET C:\\Users\\someone\\token=abc';

test('Unknown 与 Sending 状态禁发，未识别状态按禁止处理', () => {
  assert.equal(chatStatusAllowsSend('ready'), true);
  assert.equal(chatStatusAllowsSend('failed'), true);
  assert.equal(chatStatusAllowsSend('READY'), true);
  assert.equal(chatStatusAllowsSend('unknown'), false);
  assert.equal(chatStatusAllowsSend('sending'), false);
  assert.equal(chatStatusAllowsSend('pending'), false);
  assert.equal(chatStatusAllowsSend('streaming'), false);
  assert.equal(chatStatusAllowsSend(''), false);
  assert.equal(chatStatusAllowsSend(undefined), false);
});

test('有副作用操作失败时只提示确认状态，不笼统诱导重发', () => {
  const timeout = {
    code: 'host_timeout',
    message: 'host did not complete the command',
    disposition: 'unknown',
  };
  for (const operation of ['send', 'close', 'confirm_close', 'revoke']) {
    const text = describeHostFailure(operation, timeout);
    assert.match(text, /未确认/, operation);
    assert.match(text, /不要重复操作/, operation);
    assert.ok(!text.includes('重试'), `${operation} 不应提示重试：${text}`);
  }
});

test('宿主明确未执行时说明未执行，不伪装成结果未知', () => {
  const text = describeHostFailure('send', {
    code: 'queue_full',
    message: 'host command queue is full',
    disposition: 'not_executed',
  });
  assert.match(text, /未执行/);
  assert.ok(!text.includes('未确认'));
});

test('execution disposition controls retry safety independently of error codes', () => {
  assert.equal(commandDefinitelyNotExecuted({ code: 'anything', message: '', disposition: 'not_executed' }), true);
  assert.equal(commandDefinitelyNotExecuted({ code: 'queue_full', message: '', disposition: 'unknown' }), false);
  assert.equal(commandDefinitelyNotExecuted({ code: 'host_error', message: '', disposition: 'settled' }), false);

  assert.equal(isSideEffectOperation('send'), true);
  assert.equal(isSideEffectOperation('close'), true);
  assert.equal(isSideEffectOperation('confirm_close'), true);
  assert.equal(isSideEffectOperation('revoke'), true);
  assert.equal(isSideEffectOperation('draft'), false);
  assert.equal(isSideEffectOperation('load'), false);
});

test('settled host failures show their confirmed result and do not imply uncertainty', () => {
  for (const code of ['empty_input', 'input_too_large', 'display_failed']) {
    const text = describeHostFailure('send', {
      code,
      message: `backend message for ${code}`,
      disposition: 'settled',
    });
    assert.match(text, new RegExp(code));
    assert.match(text, new RegExp(`backend message for ${code}`));
    assert.match(text, /宸茬‘璁|已确认/);
    assert.doesNotMatch(text, /鏈‘璁|结果未知/);
  }
});

test('草稿同步失败给出取消语义，不诱导重发消息', () => {
  const text = describeHostFailure('draft', {
    code: 'host_timeout',
    message: 'host did not complete the command',
    disposition: 'unknown',
  });
  assert.match(text, /草稿未同步/);
  assert.match(text, /已取消/);
  assert.ok(!text.includes('重试'));
});

test('未知异常不泄露原始正文，只保留固定安全文案', () => {
  const text = describeThrownFailure('send', new Error(RAW_SECRET));
  assert.ok(!text.includes('SECRET'));
  assert.match(text, /结果未确认/);
  assert.match(text, /不要重复操作/);

  const windowText = describeThrownFailure(
    'window',
    new PrototypeBoundaryError('window_unavailable', '桌面宿主未能完成窗口命令'),
  );
  assert.match(windowText, /window_unavailable/);
  assert.match(windowText, /窗口操作/);
  assert.ok(!windowText.includes('SECRET'));
});

test('固定安全错误使用统一展示格式', () => {
  assert.equal(
    formatSafeError({ code: 'listener_release_failed', message: '无法解除桌面事件监听，请重启 Moe Buddy。', disposition: 'unknown' }),
    '无法解除桌面事件监听，请重启 Moe Buddy。（listener_release_failed）',
  );
});

test('模型配置、预设切换与记忆写入/删除属于需要确认结果的副作用操作', () => {
  for (const operation of ['model', 'preset', 'remember', 'memory_update', 'memory_delete']) {
    assert.equal(isSideEffectOperation(operation), true, operation);
    const text = describeHostFailure(operation, {
      code: 'host_timeout',
      message: 'host did not complete the command',
      disposition: 'unknown',
    });
    assert.match(text, /未确认/);
    assert.match(text, /不要重复操作/);
  }
  for (const operation of ['panel', 'memory_search', 'memory_refresh', 'diagnostics_refresh']) {
    assert.equal(isSideEffectOperation(operation), false, operation);
  }
});
