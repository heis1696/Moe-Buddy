export interface ComposerKeyInput {
  key: string;
  shiftKey: boolean;
  /** 来自 compositionstart/compositionend 的显式状态。 */
  composing: boolean;
  /** 浏览器 KeyboardEvent.isComposing；部分输入法确认键为 false，仍需看 keyCode 229。 */
  eventComposing?: boolean;
  keyCode?: number;
  canSend: boolean;
}

export type ComposerKeyAction = 'send' | 'newline' | 'ignore';

/**
 * 只决定 Enter 的语义；非 Enter 键不拦截。
 * 输入法组合期间（含 keyCode 229）Enter 交给 IME 确认候选词，不发消息。
 */
export function resolveComposerKey(input: ComposerKeyInput): ComposerKeyAction {
  if (input.key !== 'Enter') return 'ignore';
  if (input.composing || input.eventComposing === true || input.keyCode === 229) {
    return 'ignore';
  }
  if (input.shiftKey) return 'newline';
  return input.canSend ? 'send' : 'ignore';
}
