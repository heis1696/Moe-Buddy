import { IPC_CONTRACT } from './contract';
import type { BuddyExpectation, ManagementExitView, PanelName, SessionExpectation } from './types';

/** 与 Rust 宿主冻结的 IPC 名称。前端不得另造命令或事件。 */
export const IPC = {
  snapshot: 'prototype_snapshot',
  dispatch: 'prototype_dispatch',
  window: 'prototype_window',
  setHitMask: 'prototype_set_hit_mask',
  frame: 'prototype_frame',
  snapshotEvent: 'prototype://snapshot',
  frameEvent: 'prototype://frame',
  hitMaskErrorEvent: 'prototype://hit-mask-error',
  managementDraftState: 'prototype_management_draft_state',
  materialState: 'prototype_material_state',
  materialCommand: 'prototype_material_command',
  materialPixels: 'prototype_material_pixels',
  materialEvent: 'prototype://material',
} as const;

export const WINDOW_COMMANDS = [
  'show_chat',
  'hide_chat',
  'show_pet',
  'hide_pet',
  'toggle_pause',
  'start_dragging',
  'quit',
] as const;

export type WindowCommand = (typeof WINDOW_COMMANDS)[number];

export type WindowLabel = 'main' | 'pet';

/** 与 Rust 端 `open_panel`/`hide_panel` 的 panel 取值完全一致。 */
export const PANELS = ['presets', 'memory', 'diagnostics'] as const satisfies readonly PanelName[];

/** A 会话生命周期命令；仍只走 main 的受控派发入口。 */
export const SESSION_LIFECYCLE_COMMANDS = [
  'new_session',
  'switch_session',
  'archive_session',
] as const;

/** B 伙伴注册表命令；新 ID 由受信注册服务签发，前端只提交名称与人设意图。 */
export const BUDDY_LIFECYCLE_COMMANDS = [
  'create_buddy',
  'switch_buddy',
  'rename_buddy',
] as const;

/**
 * 每个业务命令发起时携带的伙伴上下文。字段名与形状是冻结契约的一部分：
 * 缺失、旧修订或另一伙伴由宿主拒绝为 NotExecuted。
 */
export interface DispatchContext {
  expectedBuddyId: string;
  expectedBuddiesRevision: number;
}

/** `prototype_dispatch` 的冻结负载：保留 `command`，另加必填 `context`。 */
export interface DispatchEnvelope {
  command: DispatchCommand;
  context: DispatchContext;
}

/** 发起时从宿主快照捕获上下文；空串哨兵表示宿主尚未签发任何伙伴。 */
export function dispatchContext(expectation: BuddyExpectation): DispatchContext {
  return {
    expectedBuddyId: expectation.expectedBuddyId,
    expectedBuddiesRevision: expectation.expectedBuddiesRevision,
  };
}

export function dispatchEnvelope(
  command: DispatchCommand,
  expectation: BuddyExpectation,
): DispatchEnvelope {
  return { command, context: dispatchContext(expectation) };
}

export interface ConfigureModelFields {
  endpoint: string;
  model: string;
  persona: string;
  allowTransmission: boolean;
  /** 字符串显式替换（空串清除）；null 仅在端点与模型均未变化时保留本次进程凭据。 */
  apiKey: string | null;
}

export type DispatchCommand =
  | {
      kind: 'create_buddy';
      label: string;
      persona: string;
      expectedManagementDraftVersion: number;
    }
  | { kind: 'switch_buddy'; id: string }
  | { kind: 'rename_buddy'; label: string; expectedManagementDraftVersion: number }
  | { kind: 'set_draft'; text: string; expectedSessionId: string | null; expectedSessionsRevision: number }
  | { kind: 'send'; expectedSessionId: string | null; expectedSessionsRevision: number }
  | { kind: 'new_session'; expectedSessionId: string | null; expectedSessionsRevision: number }
  | {
      kind: 'switch_session';
      id: string;
      expectedSessionId: string | null;
      expectedSessionsRevision: number;
    }
  | {
      kind: 'archive_session';
      id: string;
      expectedSessionId: string | null;
      expectedSessionsRevision: number;
    }
  | { kind: 'hide_chat' }
  | { kind: 'close_chat' }
  | { kind: 'confirm_close'; discard: boolean; expectedIntent: ManagementExitView['intent'] | null }
  | { kind: 'confirm_management_discard'; version: number; intent: ManagementExitView['intent'] }
  | { kind: 'cancel_management_exit'; version: number; intent: ManagementExitView['intent'] }
  | { kind: 'revoke' }
  | { kind: 'open_panel'; panel: PanelName }
  | { kind: 'hide_panel'; panel: PanelName }
  | ({ kind: 'configure_model' } & ConfigureModelFields)
  | { kind: 'select_preset'; id: string }
  | { kind: 'remember'; text: string }
  | { kind: 'search_memories'; query: string }
  | { kind: 'update_memory'; id: string; revision: number; text: string }
  | { kind: 'delete_memory'; id: string; revision: number }
  | { kind: 'refresh_memories' }
  | { kind: 'refresh_diagnostics' };

function expectationFields(expectation: SessionExpectation): {
  expectedSessionId: string | null;
  expectedSessionsRevision: number;
} {
  return {
    expectedSessionId: expectation.expectedSessionId,
    expectedSessionsRevision: expectation.expectedSessionsRevision,
  };
}

export function setDraftCommand(text: string, expectation: SessionExpectation): DispatchCommand {
  return { kind: 'set_draft', text, ...expectationFields(expectation) };
}

export function sendCommand(expectation: SessionExpectation): DispatchCommand {
  return { kind: 'send', ...expectationFields(expectation) };
}

export function newSessionCommand(expectation: SessionExpectation): DispatchCommand {
  return { kind: 'new_session', ...expectationFields(expectation) };
}

/**
 * 创建伙伴只提交有界名称、人设与表单提交时的精确管理草稿版本。实例 ID、
 * 路径、继承来源与权限都由受信服务决定，前端不提供这些字段。
 */
export function createBuddyCommand(
  label: string,
  persona: string,
  expectedManagementDraftVersion: number,
): DispatchCommand {
  return { kind: 'create_buddy', label, persona, expectedManagementDraftVersion };
}

/** 选择已有伙伴；ID 必须来自宿主注册表，前端不构造。 */
export function switchBuddyCommand(id: string): DispatchCommand {
  return { kind: 'switch_buddy', id };
}

/** 改名只提交显示名与精确管理草稿版本；不改变实例身份、人设或模型绑定。 */
export function renameBuddyCommand(
  label: string,
  expectedManagementDraftVersion: number,
): DispatchCommand {
  return { kind: 'rename_buddy', label, expectedManagementDraftVersion };
}

export function switchSessionCommand(id: string, expectation: SessionExpectation): DispatchCommand {
  return { kind: 'switch_session', id, ...expectationFields(expectation) };
}

export function archiveSessionCommand(id: string, expectation: SessionExpectation): DispatchCommand {
  return { kind: 'archive_session', id, ...expectationFields(expectation) };
}

export function hideChatCommand(): DispatchCommand {
  return { kind: 'hide_chat' };
}

export function closeChatCommand(): DispatchCommand {
  return { kind: 'close_chat' };
}

export function confirmCloseCommand(
  discard: boolean,
  expectedIntent: ManagementExitView['intent'] | null,
): DispatchCommand {
  return { kind: 'confirm_close', discard, expectedIntent };
}

export function confirmManagementDiscardCommand(
  version: number,
  intent: ManagementExitView['intent'],
): DispatchCommand {
  return { kind: 'confirm_management_discard', version, intent };
}

export function cancelManagementExitCommand(
  version: number,
  intent: ManagementExitView['intent'],
): DispatchCommand {
  return { kind: 'cancel_management_exit', version, intent };
}

export function revokeCommand(): DispatchCommand {
  return { kind: 'revoke' };
}

export function openPanelCommand(panel: PanelName): DispatchCommand {
  return { kind: 'open_panel', panel };
}

export function hidePanelCommand(panel: PanelName): DispatchCommand {
  return { kind: 'hide_panel', panel };
}

export function configureModelCommand(fields: ConfigureModelFields): DispatchCommand {
  return {
    kind: 'configure_model',
    endpoint: fields.endpoint,
    model: fields.model,
    persona: fields.persona,
    allowTransmission: fields.allowTransmission,
    apiKey: fields.apiKey,
  };
}

export function selectPresetCommand(id: string): DispatchCommand {
  return { kind: 'select_preset', id };
}

export function rememberCommand(text: string): DispatchCommand {
  return { kind: 'remember', text };
}

export function searchMemoriesCommand(query: string): DispatchCommand {
  return { kind: 'search_memories', query };
}

export function updateMemoryCommand(id: string, revision: number, text: string): DispatchCommand {
  return { kind: 'update_memory', id, revision, text };
}

export function deleteMemoryCommand(id: string, revision: number): DispatchCommand {
  return { kind: 'delete_memory', id, revision };
}

export function refreshMemoriesCommand(): DispatchCommand {
  return { kind: 'refresh_memories' };
}

export function refreshDiagnosticsCommand(): DispatchCommand {
  return { kind: 'refresh_diagnostics' };
}

/**
 * 主窗必须在发起时携带完整伙伴上下文的窗口命令。宠物窗只保留既有的
 * show_chat 聚焦与呈现/拖动，不能借窗口命令隐藏聊天、退出或读取伙伴目录。
 */
export const WINDOW_CONTEXT_COMMANDS = IPC_CONTRACT.windowContextCommands as readonly WindowCommand[];

/** `prototype_window` 的冻结负载：主窗上下文命令带 `context`，纯呈现命令不带。 */
export interface WindowCommandEnvelope {
  command: WindowCommand;
  context?: DispatchContext;
}

export function windowCommandRequiresContext(command: WindowCommand): boolean {
  return WINDOW_CONTEXT_COMMANDS.includes(command);
}

export function windowCommandPayload(
  command: WindowCommand,
  label: string | null | undefined,
  expectation?: BuddyExpectation,
): WindowCommandEnvelope {
  if (label === 'pet' && (command === 'hide_chat' || command === 'quit')) {
    throw new TypeError('pet window cannot hide chat or quit the application');
  }
  if (label === 'main' && windowCommandRequiresContext(command)) {
    if (expectation === undefined) {
      throw new TypeError('main window command requires the initiating buddy context');
    }
    // 上下文在调用方发起时捕获，这里只固化并复制，不读取当前选择。
    return { command, context: dispatchContext(expectation) };
  }
  return { command };
}

/** 读取与派发只允许 main；其他窗口只能使用窗口命令与宠物绘制命令。 */
export function dispatchSourceAllowed(label: string | null | undefined): boolean {
  return label === 'main';
}

/**
 * 会话列表、新建、切换与归档都是业务入口，只授予主窗；宠物/设置窗不能借目录
 * 读取正文或变更会话。真正的复验仍在应用层。
 */
export function sessionLifecycleAllowed(label: string | null | undefined): boolean {
  return label === 'main';
}

/**
 * 伙伴创建/选择/改名同样只授予主窗；原生窗口与托盘动作由宿主取得当前受信
 * 上下文，WebView 不能借窗口命令绕过注册表校验。
 */
export function buddyLifecycleAllowed(label: string | null | undefined): boolean {
  return label === 'main';
}

/** show/hide/pause 两个窗口都可请求；拖动仅 pet，退出与隐藏聊天仅 main。 */
export function windowCommandAllowed(
  label: string | null | undefined,
  command: WindowCommand,
): boolean {
  if (label !== 'main' && label !== 'pet') return false;
  if (command === 'start_dragging') return label === 'pet';
  if (command === 'quit') return label === 'main';
  // 宠物窗只保留既有 show_chat 聚焦与宠物呈现/拖动；隐藏聊天只能由主窗发起。
  if (command === 'hide_chat') return label === 'main';
  return true;
}
