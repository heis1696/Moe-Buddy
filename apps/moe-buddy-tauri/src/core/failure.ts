import type { PrototypeError } from './types';

/** 宿主返回的形状或调用失败时，只使用固定安全文案，绝不回显原始错误正文。 */
export class PrototypeBoundaryError extends Error {
  readonly code: string;

  constructor(code: string, message: string) {
    super(message);
    this.name = 'PrototypeBoundaryError';
    this.code = code;
  }
}

export type Operation =
  | 'load'
  | 'draft'
  | 'send'
  | 'close'
  | 'confirm_close'
  | 'management_exit'
  | 'new_session'
  | 'switch_session'
  | 'archive_session'
  | 'create_buddy'
  | 'switch_buddy'
  | 'rename_buddy'
  | 'revoke'
  | 'window'
  | 'frame'
  | 'mask'
  | 'panel'
  | 'model'
  | 'preset'
  | 'remember'
  | 'memory_search'
  | 'memory_update'
  | 'memory_delete'
  | 'memory_refresh'
  | 'diagnostics_refresh';

const OPERATION_LABELS: Readonly<Record<Operation, string>> = {
  load: '读取聊天状态',
  draft: '同步草稿',
  send: '发送消息',
  close: '关闭聊天',
  confirm_close: '确认关闭',
  management_exit: '处理管理编辑退出确认',
  new_session: '新建会话',
  switch_session: '切换会话',
  archive_session: '归档会话',
  create_buddy: '创建伙伴',
  switch_buddy: '切换伙伴',
  rename_buddy: '伙伴改名',
  revoke: '撤回授权',
  window: '窗口操作',
  frame: '读取动画帧',
  mask: '同步宠物命中区域',
  panel: '切换管理面板',
  model: '应用模型配置',
  preset: '切换聊天预设',
  remember: '保存记忆',
  memory_search: '检索记忆',
  memory_update: '保存记忆修改',
  memory_delete: '删除记忆',
  memory_refresh: '刷新记忆',
  diagnostics_refresh: '刷新诊断',
};

/** 有副作用的操作：结果不明确时只提示确认状态，不笼统引导再次发送。 */
const SIDE_EFFECT_OPERATIONS: ReadonlySet<Operation> = new Set([
  'send',
  'close',
  'confirm_close',
  'management_exit',
  'new_session',
  'switch_session',
  'archive_session',
  'create_buddy',
  'switch_buddy',
  'rename_buddy',
  'revoke',
  'model',
  'preset',
  'remember',
  'memory_update',
  'memory_delete',
]);

export function isSideEffectOperation(operation: Operation): boolean {
  return SIDE_EFFECT_OPERATIONS.has(operation);
}

/** 固定安全操作名，供会话锁说明等界面文案使用，不回显宿主原始错误。 */
export function operationLabel(operation: Operation): string {
  return OPERATION_LABELS[operation];
}

/** 宿主提供的处置证据决定命令是否执行；错误码本身不参与推断。 */
export function commandDefinitelyNotExecuted(error: PrototypeError): boolean {
  return error.disposition === 'not_executed';
}

function errorDetail(error: PrototypeError): string {
  const message = error.message.trim();
  const code = error.code.trim().length > 0 ? error.code.trim() : '未知';
  return message.length > 0 ? `${message}（${code}）` : `桌面宿主返回错误（${code}）`;
}

export function describeHostFailure(operation: Operation, error: PrototypeError): string {
  const label = OPERATION_LABELS[operation];
  const detail = errorDetail(error);
  if (error.disposition === 'not_executed') {
    return `桌面宿主未执行本次${label}：${detail}`;
  }
  if (error.disposition === 'settled') {
    return `桌面宿主已确认本次${label}处理：${detail}`;
  }
  if (SIDE_EFFECT_OPERATIONS.has(operation)) {
    return `桌面宿主未确认本次${label}结果：${detail}。请先确认当前状态，不要重复操作。`;
  }
  if (operation === 'draft') {
    return `草稿未同步到桌面宿主：${detail}。本次操作已取消。`;
  }
  return `桌面宿主返回错误：${detail}`;
}

export function describeThrownFailure(operation: Operation, error: unknown): string {
  const label = OPERATION_LABELS[operation];
  if (error instanceof PrototypeBoundaryError) {
    return `桌面宿主未能完成${label}：${error.message}（${error.code}）`;
  }
  if (SIDE_EFFECT_OPERATIONS.has(operation)) {
    return `桌面宿主未能完成${label}，结果未确认；请先确认当前状态，不要重复操作。`;
  }
  return `桌面宿主未能完成${label}。`;
}

/** 固定安全错误（如监听解除失败）的统一展示格式。 */
export function formatSafeError(error: PrototypeError): string {
  const message = error.message.trim();
  const code = error.code.trim();
  if (message.length > 0 && code.length > 0) return `${message}（${code}）`;
  if (message.length > 0) return message;
  return `桌面宿主返回固定错误（${code.length > 0 ? code : '未知'}）`;
}
