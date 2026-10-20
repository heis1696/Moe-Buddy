const test = require('node:test');
const assert = require('node:assert/strict');

const {
  buildConfigureModelCommand,
  emptyModelForm,
  endpointOrModelChanged,
  modelFormAfterApply,
  modelFormDirty,
  modelFormFromSettings,
  requiresTransmissionReset,
} = require('../.test-build/modelConfig.js');

function settings(overrides = {}) {
  return {
    endpoint: 'https://example.invalid/v1',
    model: 'synthetic',
    persona: '苔',
    allowTransmission: true,
    hasApiKey: true,
    ...overrides,
  };
}

function form(overrides = {}) {
  return {
    ...emptyModelForm(),
    endpoint: 'https://example.invalid/v1',
    model: 'synthetic',
    persona: '苔',
    allowTransmission: true,
    ...overrides,
  };
}

test('端点与模型不变时以 null 保留本次进程凭据', () => {
  const built = buildConfigureModelCommand(settings(), form());
  assert.equal(built.ok, true);
  assert.deepEqual(built.command, {
    kind: 'configure_model',
    endpoint: 'https://example.invalid/v1',
    model: 'synthetic',
    persona: '苔',
    allowTransmission: true,
    apiKey: null,
  });
});

test('尚未配置模型时 null 等同无凭据', () => {
  const built = buildConfigureModelCommand(
    null,
    form({ endpoint: 'http://127.0.0.1:9/v1', model: 'local', allowTransmission: false }),
  );
  assert.equal(built.ok, true);
  assert.equal(built.command.apiKey, null);
});

test('端点或模型变化时拒绝 null，要求显式重新输入或清除', () => {
  const changedEndpoint = buildConfigureModelCommand(
    settings(),
    form({ endpoint: 'https://other.invalid/v1' }),
  );
  assert.equal(changedEndpoint.ok, false);
  assert.equal(changedEndpoint.code, 'credential_required');
  assert.match(changedEndpoint.message, /重新输入|清除/);

  const changedModel = buildConfigureModelCommand(settings(), form({ model: 'other' }));
  assert.equal(changedModel.ok, false);
  assert.equal(changedModel.code, 'credential_required');

  const trimmedSame = buildConfigureModelCommand(
    settings(),
    form({ endpoint: '  https://example.invalid/v1  ', model: '  synthetic  ' }),
  );
  assert.equal(trimmedSame.ok, true);
  assert.equal(trimmedSame.command.apiKey, null);
});

test('非空字符串显式替换，空串显式清除', () => {
  const replaced = buildConfigureModelCommand(
    settings(),
    form({ endpoint: 'https://other.invalid/v1', apiKeyInput: 'new-secret' }),
  );
  assert.equal(replaced.ok, true);
  assert.equal(replaced.command.apiKey, 'new-secret');
  assert.equal(replaced.command.endpoint, 'https://other.invalid/v1');

  const cleared = buildConfigureModelCommand(settings(), form({ clearApiKey: true }));
  assert.equal(cleared.ok, true);
  assert.equal(cleared.command.apiKey, '');

  const clearedAfterChange = buildConfigureModelCommand(
    settings(),
    form({ model: 'other', clearApiKey: true }),
  );
  assert.equal(clearedAfterChange.ok, true);
  assert.equal(clearedAfterChange.command.apiKey, '');
});

test('输入框里的新密钥优先于清除勾选，不把密码写入非凭据字段', () => {
  const built = buildConfigureModelCommand(
    settings(),
    form({ apiKeyInput: 'typed', clearApiKey: true }),
  );
  assert.equal(built.ok, true);
  assert.equal(built.command.apiKey, 'typed');
  assert.deepEqual(Object.keys(built.command).sort(), [
    'allowTransmission',
    'apiKey',
    'endpoint',
    'kind',
    'model',
    'persona',
  ]);
  assert.ok(!('hasApiKey' in built.command));
});

test('空端点、空模型或空人设在本地明确拒绝，不派发半成品配置', () => {
  assert.equal(buildConfigureModelCommand(null, form({ endpoint: '   ' })).code, 'invalid_model_config');
  assert.equal(buildConfigureModelCommand(null, form({ model: '' })).code, 'invalid_model_config');
  const emptyPersona = buildConfigureModelCommand(null, form({ persona: ' \n\t ' }));
  assert.equal(emptyPersona.code, 'invalid_model_config');
  assert.match(emptyPersona.message, /人设不能为空/);
});

test('未配置模型时用活动档案人设作为基线，空表单不覆盖已绑定人设', () => {
  const bound = '伙伴已绑定的人设\n第二行';
  const fromPersona = modelFormFromSettings(null, bound);
  assert.equal(fromPersona.persona, bound);
  assert.equal(fromPersona.endpoint, '');
  assert.equal(modelFormDirty(null, fromPersona, bound), false);
  assert.equal(modelFormDirty(null, emptyModelForm(), bound), true);
  assert.equal(modelFormAfterApply(null, bound).persona, bound);

  const built = buildConfigureModelCommand(
    null,
    form({ endpoint: 'http://127.0.0.1:9/v1', model: 'local', persona: bound }),
  );
  assert.equal(built.ok, true);
  assert.equal(built.command.persona, bound);
});

test('应用成功后密码框清空并回到宿主基线', () => {
  const after = modelFormAfterApply(settings());
  assert.equal(after.apiKeyInput, '');
  assert.equal(after.clearApiKey, false);
  assert.equal(after.transmissionTouched, false);
  assert.equal(after.endpoint, 'https://example.invalid/v1');
  assert.equal(after.allowTransmission, true);
  assert.deepEqual(modelFormFromSettings(null), emptyModelForm());
});

test('脏表单检测覆盖配置、密码框与清除勾选', () => {
  const baseline = settings();
  assert.equal(modelFormDirty(baseline, form()), false);
  assert.equal(modelFormDirty(baseline, form({ endpoint: 'https://other.invalid/v1' })), true);
  assert.equal(modelFormDirty(baseline, form({ persona: '新的人设' })), true);
  assert.equal(modelFormDirty(baseline, form({ allowTransmission: false })), true);
  assert.equal(modelFormDirty(baseline, form({ apiKeyInput: 'typed' })), true);
  assert.equal(modelFormDirty(baseline, form({ clearApiKey: true })), true);
  assert.equal(modelFormDirty(null, emptyModelForm()), false);
});

test('端点/模型变化后旧发送许可不自动沿用，用户主动重选后不再重置', () => {
  const baseline = settings({ allowTransmission: true });
  const changed = form({ endpoint: 'https://other.invalid/v1' });
  assert.equal(endpointOrModelChanged(baseline, changed), true);
  assert.equal(requiresTransmissionReset(baseline, changed), true);

  const retouched = form({ endpoint: 'https://other.invalid/v1', transmissionTouched: true });
  assert.equal(requiresTransmissionReset(baseline, retouched), false);

  assert.equal(requiresTransmissionReset(baseline, form()), false);
  assert.equal(
    requiresTransmissionReset(settings({ allowTransmission: false }), changed),
    false,
  );
  assert.equal(endpointOrModelChanged(null, changed), false);
});
