const test = require('node:test');
const assert = require('node:assert/strict');

const { PanelSwitchGuard } = require('../.test-build/panelGuard.js');

test('无未保存编辑时面板切换直接放行', () => {
  const guard = new PanelSwitchGuard();
  assert.equal(guard.request('memory', false), true);
  assert.equal(guard.pending, null);
});

test('有未保存编辑时挂起切换，取消后保留原面板与草稿', () => {
  const guard = new PanelSwitchGuard();
  assert.equal(guard.request('memory', true), false);
  assert.equal(guard.pending, 'memory');
  guard.cancel();
  assert.equal(guard.pending, null);
});

test('确认离开只消费一次挂起目标', () => {
  const guard = new PanelSwitchGuard();
  guard.request('close', true);
  assert.equal(guard.confirm(), 'close');
  assert.equal(guard.confirm(), null);
});

test('新目标覆盖挂起目标，仍然需要再次确认', () => {
  const guard = new PanelSwitchGuard();
  assert.equal(guard.request('memory', true), false);
  assert.equal(guard.request('diagnostics', true), false);
  assert.equal(guard.pending, 'diagnostics');
  assert.equal(guard.request('presets', false), true);
  assert.equal(guard.pending, null);
});

test('关闭面板同样走未保存编辑确认', () => {
  const guard = new PanelSwitchGuard();
  assert.equal(guard.request('close', true), false);
  assert.equal(guard.pending, 'close');
  guard.cancel();
  assert.equal(guard.pending, null);
});
