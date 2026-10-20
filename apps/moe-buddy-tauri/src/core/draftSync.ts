import type { SnapshotResponse } from './types';

export type DraftDispatch = (text: string) => Promise<SnapshotResponse>;

export type DraftSettledHandler = (
  text: string,
  response: SnapshotResponse | null,
  error: unknown,
) => void;

export interface DraftSyncOptions {
  dispatch: DraftDispatch;
  onSettled?: DraftSettledHandler;
}

/**
 * 有界合并草稿同步队列：
 * - 至多一个在途请求，其余输入只保留最后一份待发文本，不累积每次输入的副本；
 * - 请求失败后不自动重试同一文本，等待下一次显式 flush/enqueue；
 * - 撤权或卸载时可丢弃尚未发出的草稿（discardPending/cancel）。
 */
export class DraftSyncQueue {
  private readonly dispatchDraft: DraftDispatch;
  private readonly onSettled: DraftSettledHandler | undefined;
  private pending: string | null = null;
  private inFlight: string | null = null;
  private desired: string | null = null;
  private confirmed: string | null = null;
  private healthy = true;
  private cancelled = false;
  private pumping = false;
  private waiters: Array<(synced: boolean) => void> = [];

  constructor(options: DraftSyncOptions) {
    this.dispatchDraft = options.dispatch;
    this.onSettled = options.onSettled;
  }

  get isCancelled(): boolean {
    return this.cancelled;
  }

  get hasPendingWork(): boolean {
    return this.inFlight !== null || this.pending !== null;
  }

  get latestRequested(): string | null {
    return this.desired;
  }

  get pendingText(): string | null {
    return this.pending;
  }

  enqueue(text: string): void {
    if (this.cancelled) return;
    if (this.inFlight === text) {
      // 用户回改到正在发送的文本时，在途请求已经承载最新意图。必须丢弃暂存的
      // 旧文本，否则它会在在途请求完成后被继续派发，覆盖用户回改后的正文。
      // 在途请求若失败，不自动补发同一文本；等显式 flushLatest/enqueue 再重试。
      this.pending = null;
      this.desired = text;
      this.healthy = false;
      return;
    }
    if (this.pending === text && this.desired === text) return;
    if (!this.hasPendingWork && this.healthy && this.desired === text && this.confirmed === text) {
      return;
    }
    this.desired = text;
    this.pending = text;
    this.healthy = false;
    void this.pump();
  }

  flushLatest(text: string): Promise<boolean> {
    this.enqueue(text);
    return this.flush();
  }

  /** 等待当前合并批次的最后一个请求完成；失败或已取消时返回 false。 */
  flush(): Promise<boolean> {
    if (this.cancelled) return Promise.resolve(false);
    if (!this.hasPendingWork) return Promise.resolve(this.isSynced());
    return new Promise<boolean>((resolve) => {
      this.waiters.push(resolve);
      void this.pump();
    });
  }

  isSynced(): boolean {
    return (
      !this.cancelled &&
      this.healthy &&
      !this.hasPendingWork &&
      this.confirmed === this.desired
    );
  }

  /** 宿主在其他响应里给出了新的草稿基线（例如发送成功后清空草稿）。 */
  adoptHostDraft(text: string): void {
    if (this.cancelled || this.hasPendingWork) return;
    this.desired = text;
    this.confirmed = text;
    this.healthy = true;
    this.settleWaiters();
  }

  /**
   * 伙伴作用域推进后保留本地未同步正文，但不把旧队列的待发编辑自动换成新
   * context 重发。队列保持未同步，下一次用户编辑或显式 flushLatest 才重新派发。
   */
  retainUnsynced(text: string): void {
    if (this.cancelled || this.hasPendingWork) return;
    this.desired = text;
    this.confirmed = null;
    this.healthy = false;
  }

  /** 撤权前取消尚未派发的草稿；不影响之后重新开始编辑。 */
  discardPending(): void {
    if (!this.hasPendingWork) return;
    this.pending = null;
    this.desired = null;
    this.confirmed = null;
    this.healthy = false;
    this.settleWaiters();
  }

  /** 窗口卸载或已确认撤权后永久取消本队列。 */
  cancel(): void {
    if (this.cancelled) return;
    this.cancelled = true;
    this.pending = null;
    this.desired = null;
    this.confirmed = null;
    this.healthy = false;
    this.settleWaiters();
  }

  private async pump(): Promise<void> {
    if (this.pumping) return;
    this.pumping = true;
    try {
      while (!this.cancelled && this.pending !== null) {
        const text = this.pending;
        this.pending = null;
        this.inFlight = text;
        let response: SnapshotResponse | null = null;
        let failure: unknown = null;
        try {
          response = await this.dispatchDraft(text);
        } catch (error) {
          failure = error;
        }
        const cancelled = this.cancelled;
        this.inFlight = null;
        if (cancelled) break;
        if (response !== null && response.error === null) {
          this.confirmed = text;
          if (this.desired === text && this.pending === null) this.healthy = true;
        } else {
          this.healthy = false;
        }
        this.onSettled?.(text, failure === null ? response : null, failure);
        if (this.pending === null) break;
      }
    } finally {
      this.pumping = false;
      this.inFlight = null;
      this.settleWaiters();
    }
  }

  private settleWaiters(): void {
    if (this.hasPendingWork) return;
    const waiters = this.waiters;
    this.waiters = [];
    const synced = this.isSynced();
    for (const resolve of waiters) resolve(synced);
  }
}
