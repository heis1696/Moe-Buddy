const test = require('node:test');
const assert = require('node:assert/strict');
const {
  closeDialogBody,
  closeDialogConfirmLabel,
  closeDialogTitle,
  managementExitBody,
  managementExitConfirmLabel,
} = require('../.test-build/exitCopy.js');

// 领队决定的退出语义：CloseChat 只处理当前会话草稿；Quit 丢弃所有会话草稿
// 以及管理面板未保存的编辑。这里只校验用户可见承诺，不替代真实窗口与宿主保护。

test('关闭聊天只承诺清理当前会话草稿，不声称丢弃其他会话或管理编辑', () => {
  for (const intent of [null, 'close_chat']) {
    const body = closeDialogBody(intent);
    assert.match(body, /当前会话草稿/);
    assert.match(body, /其他会话草稿保留/);
    assert.equal(body.includes('所有会话草稿'), false, `close copy must not promise all sessions: ${body}`);
    assert.equal(body.includes('管理面板'), false, `close copy must not touch management edits: ${body}`);
    assert.equal(closeDialogTitle(intent), '关闭聊天？');
    assert.equal(closeDialogConfirmLabel(intent), '丢弃草稿并关闭');
  }
});

test('退出应用承诺丢弃所有会话草稿与管理未保存编辑', () => {
  const body = closeDialogBody('quit_application');
  assert.match(body, /所有会话草稿/);
  assert.match(body, /管理面板未保存的编辑/);
  assert.match(body, /退出应用/);
  assert.equal(closeDialogTitle('quit_application'), '退出 Moe Buddy？');
  assert.equal(closeDialogConfirmLabel('quit_application'), '丢弃草稿并退出');
});

test('管理编辑叠加关闭/退出时按意图说明清理范围并保留既有确认形状', () => {
  const quitDirty = managementExitBody(true, 'quit_application');
  const quitClean = managementExitBody(false, 'quit_application');
  const closeDirty = managementExitBody(true, 'close_chat');
  const closeClean = managementExitBody(false, 'close_chat');

  for (const body of [quitDirty, quitClean]) {
    assert.match(body, /所有会话草稿/);
    assert.match(body, /退出应用/);
  }
  for (const body of [closeDirty, closeClean]) {
    assert.match(body, /当前会话草稿/);
    assert.match(body, /关闭聊天/);
    assert.equal(body.includes('所有会话草稿'), false, `close copy must stay scoped: ${body}`);
  }
  assert.match(quitDirty, /放弃这些修改/);
  assert.match(closeDirty, /放弃这些修改/);
  assert.equal(quitClean.includes('未保存的修改'), false);
  assert.equal(closeClean.includes('未保存的修改'), false);

  // 按钮形状沿用 D 阶段既有文案，不因范围调整而改名。
  assert.equal(managementExitConfirmLabel(true), '放弃编辑并继续');
  assert.equal(managementExitConfirmLabel(false), '继续');
});
