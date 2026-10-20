import type { HitMaskPayload } from './types';

export type MaskDispatch = (mask: HitMaskPayload) => Promise<void>;

export interface MaskSettleResult {
  ok: boolean;
  error: unknown;
  /** 该结果返回前已有更新的掩码排队，界面不应据此清除/覆盖最新错误。 */
  superseded: boolean;
}

export type MaskSettledHandler = (mask: HitMaskPayload, result: MaskSettleResult) => void;

export interface MaskSyncOptions {
  dispatch: MaskDispatch;
  onSettled?: MaskSettledHandler;
}

/**
 * 有界命中掩码安装队列：至多一个在途安装，其余尺寸/DPI 变化只保留最新一份。
 * 连续变化不会把旧网格排成一长串继续安装；卸载时可丢弃尚未发出的更新。
 */
export class MaskSyncQueue {
  private readonly dispatchMask: MaskDispatch;
  private readonly onSettled: MaskSettledHandler | undefined;
  private pending: HitMaskPayload | null = null;
  private inFlight: HitMaskPayload | null = null;
  private pumping = false;
  private cancelled = false;

  constructor(options: MaskSyncOptions) {
    this.dispatchMask = options.dispatch;
    this.onSettled = options.onSettled;
  }

  get isCancelled(): boolean {
    return this.cancelled;
  }

  get hasPendingWork(): boolean {
    return this.inFlight !== null || this.pending !== null;
  }

  get pendingMask(): HitMaskPayload | null {
    return this.pending;
  }

  get inFlightMask(): HitMaskPayload | null {
    return this.inFlight;
  }

  enqueue(mask: HitMaskPayload): void {
    if (this.cancelled) return;
    this.pending = mask;
    void this.pump();
  }

  /** 丢弃尚未发出的更新；在途 invoke 无法取消，但其结果由调用方按 disposed 忽略。 */
  cancel(): void {
    if (this.cancelled) return;
    this.cancelled = true;
    this.pending = null;
  }

  private async pump(): Promise<void> {
    if (this.pumping || this.cancelled) return;
    this.pumping = true;
    try {
      while (!this.cancelled && this.pending !== null) {
        const mask = this.pending;
        this.pending = null;
        this.inFlight = mask;
        let failure: unknown = null;
        try {
          await this.dispatchMask(mask);
        } catch (error) {
          failure = error;
        }
        const superseded = this.pending !== null || this.cancelled;
        this.inFlight = null;
        if (!this.cancelled) {
          this.onSettled?.(mask, { ok: failure === null, error: failure, superseded });
        }
      }
    } finally {
      this.pumping = false;
      this.inFlight = null;
    }
  }
}
