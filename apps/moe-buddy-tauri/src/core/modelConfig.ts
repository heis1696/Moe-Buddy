import type { ModelSettingsView } from './types';
import { configureModelCommand, type DispatchCommand } from './protocol';

/**
 * 模型配置表单只保存待提交值；API Key 输入是密码框内的短时状态，
 * 不进入快照、IndexedDB/localStorage 或日志，应用成功后立即清空。
 */
export interface ModelFormState {
  endpoint: string;
  model: string;
  persona: string;
  allowTransmission: boolean;
  apiKeyInput: string;
  /** 用户显式要求以空串清除本次进程凭据。 */
  clearApiKey: boolean;
  /** 用户是否主动改过发送许可；用于端点/模型变化时要求重新确认。 */
  transmissionTouched: boolean;
}

export type ConfigureModelBuild =
  | { ok: true; command: DispatchCommand }
  | { ok: false; code: 'invalid_model_config' | 'credential_required'; message: string };

export function emptyModelForm(): ModelFormState {
  return {
    endpoint: '',
    model: '',
    persona: '',
    allowTransmission: false,
    apiKeyInput: '',
    clearApiKey: false,
    transmissionTouched: false,
  };
}

/**
 * 配置表单基线。模型配置未建立时，活动档案已绑定的人设仍是权威基线：
 * 不能用空表单覆盖它，也不能把别的伙伴人设搬过来。
 */
export function modelFormFromSettings(
  settings: ModelSettingsView | null,
  activePersona: string | null = null,
): ModelFormState {
  if (settings === null) {
    return { ...emptyModelForm(), persona: activePersona ?? '' };
  }
  return {
    endpoint: settings.endpoint,
    model: settings.model,
    persona: settings.persona,
    allowTransmission: settings.allowTransmission,
    apiKeyInput: '',
    clearApiKey: false,
    transmissionTouched: false,
  };
}

/** 应用成功后的表单基线：密码框清空，保留宿主返回的非凭据配置。 */
export function modelFormAfterApply(
  settings: ModelSettingsView | null,
  activePersona: string | null = null,
): ModelFormState {
  return modelFormFromSettings(settings, activePersona);
}

/** 端点或模型相对已配置值发生变化；空串按未配置处理，避免误用旧凭据。 */
export function endpointOrModelChanged(
  settings: ModelSettingsView | null,
  form: ModelFormState,
): boolean {
  if (settings === null) return false;
  return form.endpoint.trim() !== settings.endpoint || form.model.trim() !== settings.model;
}

export function modelFormDirty(
  settings: ModelSettingsView | null,
  form: ModelFormState,
  activePersona: string | null = null,
): boolean {
  const baseline = modelFormFromSettings(settings, activePersona);
  return (
    form.endpoint !== baseline.endpoint ||
    form.model !== baseline.model ||
    form.persona !== baseline.persona ||
    form.allowTransmission !== baseline.allowTransmission ||
    form.apiKeyInput.length > 0 ||
    form.clearApiKey
  );
}

/**
 * 端点或模型变化后，旧发送许可不能自动沿用；前端先取消勾选，
 * 用户必须主动重新勾选（之后不再因继续输入被反复取消）。
 */
export function requiresTransmissionReset(
  settings: ModelSettingsView | null,
  form: ModelFormState,
): boolean {
  return (
    !form.transmissionTouched &&
    form.allowTransmission &&
    settings?.allowTransmission === true &&
    endpointOrModelChanged(settings, form)
  );
}

/**
 * 冻结的 apiKey 语义：
 * - 非空字符串显式替换；空串显式清除；
 * - null 仅在端点和模型都未变化时保留本次进程凭据；未配置模型时等同无凭据；
 * - 端点或模型变化时 null 会被宿主拒绝，因此前端要求重新输入或显式清除。
 */
export function buildConfigureModelCommand(
  settings: ModelSettingsView | null,
  form: ModelFormState,
): ConfigureModelBuild {
  const endpoint = form.endpoint.trim();
  const model = form.model.trim();
  if (endpoint.length === 0 || model.length === 0) {
    return {
      ok: false,
      code: 'invalid_model_config',
      message: '端点与模型都不能为空。',
    };
  }
  // 人设属于档案绑定：空白表单不能覆盖已有人设，也不提交非法控制字符。
  if (form.persona.trim().length === 0) {
    return {
      ok: false,
      code: 'invalid_model_config',
      message: '人设不能为空；未配置模型时请保留当前档案人设。',
    };
  }

  const unchanged = settings !== null && endpoint === settings.endpoint && model === settings.model;
  let apiKey: string | null;
  if (form.apiKeyInput.length > 0) {
    apiKey = form.apiKeyInput;
  } else if (form.clearApiKey) {
    apiKey = '';
  } else if (settings === null || unchanged) {
    apiKey = null;
  } else {
    return {
      ok: false,
      code: 'credential_required',
      message:
        '端点或模型已变化，旧 API Key 不会自动沿用。请重新输入 API Key，或勾选“本次不使用 / 清除 API Key”。',
    };
  }

  return {
    ok: true,
    command: configureModelCommand({
      endpoint,
      model,
      persona: form.persona,
      allowTransmission: form.allowTransmission,
      apiKey,
    }),
  };
}
