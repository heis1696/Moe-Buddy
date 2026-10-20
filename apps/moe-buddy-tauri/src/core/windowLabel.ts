export type WindowPage = 'main' | 'pet' | 'unknown';

/** 同一 index.html 只按 Tauri 窗口 label 分流；其他 label 不猜测业务页面。 */
export function resolveWindowPage(label: string | null | undefined): WindowPage {
  if (label === 'main') return 'main';
  if (label === 'pet') return 'pet';
  return 'unknown';
}
