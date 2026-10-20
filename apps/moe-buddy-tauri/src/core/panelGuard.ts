import type { PanelName } from './types';

export type PanelTarget = PanelName | 'close';

/**
 * 面板切换只做展示层仲裁：有未保存编辑时必须先得到显式确认；
 * 取消后保持原面板和全部草稿，确认后才丢弃并继续目标操作。
 */
export class PanelSwitchGuard {
  private pendingTarget: PanelTarget | null = null;

  get pending(): PanelTarget | null {
    return this.pendingTarget;
  }

  /** 返回 true 表示可直接切换；false 表示已挂起，等待 confirm/cancel。 */
  request(target: PanelTarget, dirty: boolean): boolean {
    if (!dirty) {
      this.pendingTarget = null;
      return true;
    }
    this.pendingTarget = target;
    return false;
  }

  cancel(): void {
    this.pendingTarget = null;
  }

  /** 只有挂起中的目标会被返回一次；调用方随后负责丢弃草稿。 */
  confirm(): PanelTarget | null {
    const target = this.pendingTarget;
    this.pendingTarget = null;
    return target;
  }
}
