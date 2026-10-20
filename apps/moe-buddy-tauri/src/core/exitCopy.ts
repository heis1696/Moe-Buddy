/**
 * 关闭/退出确认文案的单一来源。
 *
 * 领队决定的语义边界：
 * - CloseChat 只处理当前会话草稿；其他会话草稿保留。
 * - Quit 丢弃所有会话草稿以及管理面板未保存的编辑。
 *
 * 文案只描述用户的可见承诺，不推导权限、不解除 Unknown，也不替代宿主的
 * closeIntent/managementExit 形状。Rust 端的对应保护由另一单承接。
 */

export type ExitIntent = 'close_chat' | 'quit_application';

/** 普通关闭确认（未叠加管理面板退出）的标题。 */
export function closeDialogTitle(intent: ExitIntent | null): string {
  return intent === 'quit_application' ? '退出 Moe Buddy？' : '关闭聊天？';
}

/** 普通关闭确认正文：明确区分“当前草稿”和“所有会话草稿”。 */
export function closeDialogBody(intent: ExitIntent | null): string {
  if (intent === 'quit_application') {
    return '取消会保留当前草稿与界面；丢弃会清空所有会话草稿、放弃管理面板未保存的编辑并退出应用。';
  }
  return '取消会保留当前草稿与界面；丢弃会清空当前会话草稿并关闭聊天，其他会话草稿保留。';
}

/** 普通关闭确认按钮文案。 */
export function closeDialogConfirmLabel(intent: ExitIntent | null): string {
  return intent === 'quit_application' ? '丢弃草稿并退出' : '丢弃草稿并关闭';
}

/**
 * 管理编辑与关闭/退出意图叠加时的确认正文。
 * 管理编辑仍是独立同步与丢弃回执，文案只说明本次关闭/退出的清理范围。
 */
export function managementExitBody(dirty: boolean, intent: ExitIntent): string {
  if (intent === 'quit_application') {
    return dirty
      ? '管理面板有未保存的修改。取消会保留编辑；确认会放弃这些修改、清空所有会话草稿并退出应用。'
      : '管理编辑已清理。确认后会清空所有会话草稿并退出应用。';
  }
  return dirty
    ? '管理面板有未保存的修改。取消会保留编辑；确认会放弃这些修改、清空当前会话草稿并关闭聊天。'
    : '管理编辑已清理。确认后会清空当前会话草稿并关闭聊天。';
}

/** 管理编辑退出确认按钮文案；沿用既有“放弃编辑并继续/继续”形状。 */
export function managementExitConfirmLabel(dirty: boolean): string {
  return dirty ? '放弃编辑并继续' : '继续';
}
