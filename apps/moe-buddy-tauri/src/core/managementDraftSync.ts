import { buddyExpectationMatches } from './buddy';
import type { BuddyExpectation, ManagementDraftAck, ManagementDraftKind } from './types';

export interface ManagementDraftStatus {
  version: number;
  dirty: boolean;
  kind: ManagementDraftKind;
}

export interface ManagementDiscardReceipt {
  managementDiscardedVersion: number | null;
}

/** 管理编辑同步的完整作用域：伙伴 ID 与发起时的选择修订共同绑定。 */
export type ManagementDraftScope = BuddyExpectation;

export type ManagementDraftDispatch = (
  version: number,
  dirty: boolean,
  kind: ManagementDraftKind,
) => Promise<ManagementDraftAck>;

const MAX_SAFE_VERSION = Number.MAX_SAFE_INTEGER;
const CLEAN: ManagementDraftStatus = { version: 0, dirty: false, kind: 'none' };

/** 冻结契约：`none` 只与 clean 组合；其余种类只在 dirty 时有效。 */
export function isManagementDraftStatus(
  value: ManagementDraftStatus,
): boolean {
  if (!Number.isSafeInteger(value.version) || value.version < 0) return false;
  if (typeof value.dirty !== 'boolean') return false;
  if (!['none', 'general', 'buddy_create', 'buddy_rename'].includes(value.kind)) return false;
  if (value.dirty) return value.kind !== 'none';
  return value.kind === 'none';
}

/**
 * 管理编辑状态同步：只发送版本、dirty 与种类，一个在途更新加一个合并后的最新值。
 *
 * B 伙伴注册表：
 * - 队列在创建时绑定**完整作用域**（伙伴 ID + `expectedBuddiesRevision`）。
 *   `record`/`adopt`/丢弃水位消费都必须与完整作用域匹配，旧伙伴或同一伙伴的旧
 *   选择修订都不能混用；
 * - 派发回调只使用构造时捕获的作用域，绝不在派发瞬间读取“当前作用域”。选择
 *   修订推进后，调用方必须取消旧实例并用新作用域重建；旧队列里的待发编辑不得
 *   被换成新修订（`cancel` 会丢弃待发合并值，但不影响调用方保留的表单正文）；
 * - 种类与版本共同标识一份受保护表单：创建/改名提交只消费同种类且精确
 *   版本的回执，其他编辑不会被一次保存顺带清掉。
 */
export class ManagementDraftSync {
  /** 构造时冻结的完整作用域；派发与回执只以此为准。 */
  readonly scope: ManagementDraftScope;
  private readonly dispatch: ManagementDraftDispatch;
  private readonly onFailure: (() => void) | undefined;
  private readonly onChange: (() => void) | undefined;
  private desired: ManagementDraftStatus = { ...CLEAN };
  private confirmed: ManagementDraftStatus | null = { ...CLEAN };
  private pending: ManagementDraftStatus | null = null;
  private running = false;
  private cancelled = false;
  private waiters: Array<(synced: boolean) => void> = [];
  private lastConsumedDiscardedVersion = -1;

  constructor(
    scope: ManagementDraftScope,
    dispatch: ManagementDraftDispatch,
    onFailure?: () => void,
    onChange?: () => void,
  ) {
    this.scope = { ...scope };
    this.dispatch = dispatch;
    this.onFailure = onFailure;
    this.onChange = onChange;
  }

  private notify(): void {
    this.onChange?.();
  }

  get isCancelled(): boolean {
    return this.cancelled;
  }

  get version(): number {
    return this.desired.version;
  }

  get dirty(): boolean {
    return this.desired.dirty;
  }

  get kind(): ManagementDraftKind {
    return this.desired.kind;
  }

  get isSynced(): boolean {
    if (this.cancelled) return false;
    return (
      this.pending === null &&
      !this.running &&
      this.confirmed?.version === this.desired.version &&
      this.confirmed.dirty === this.desired.dirty &&
      this.confirmed.kind === this.desired.kind
    );
  }

  /** 完整作用域比较：ID 与选择修订任一不同都不是同一作用域。 */
  matchesScope(scope: ManagementDraftScope): boolean {
    return buddyExpectationMatches(this.scope, scope);
  }

  /** 只有同一完整作用域的权威状态才可被采纳，旧同步不覆盖新作用域。 */
  adopt(status: ManagementDraftStatus, scope: ManagementDraftScope = this.scope): boolean {
    if (this.cancelled) return false;
    if (!this.matchesScope(scope)) return false;
    if (!isManagementDraftStatus(status)) return false;
    if (this.running || this.pending !== null || status.version < this.desired.version) return false;
    if (status.version <= this.lastConsumedDiscardedVersion && status.dirty) return false;
    this.desired = { ...status };
    this.confirmed = { ...status };
    this.notify();
    return true;
  }

  /**
   * 消费具体版本的丢弃回执。版本、完整作用域与本地同步状态必须同时匹配；
   * 旧伙伴或旧选择修订的丢弃回执不清当前编辑，也不解除其他 Unknown。
   */
  consumeDiscardedVersion(
    version: number,
    scope: ManagementDraftScope = this.scope,
  ): boolean {
    if (
      this.cancelled ||
      !this.matchesScope(scope) ||
      version <= this.lastConsumedDiscardedVersion ||
      this.desired.version !== version ||
      !this.desired.dirty ||
      this.pending !== null ||
      this.running ||
      this.confirmed?.version !== version ||
      !this.confirmed.dirty
    ) {
      return false;
    }
    this.lastConsumedDiscardedVersion = version;
    this.desired = { version, dirty: false, kind: 'none' };
    this.confirmed = { version, dirty: false, kind: 'none' };
    this.notify();
    return true;
  }

  record(
    dirty: boolean,
    kind: ManagementDraftKind = dirty ? 'general' : 'none',
    scope: ManagementDraftScope = this.scope,
  ): boolean {
    if (this.cancelled) return false;
    if (!this.matchesScope(scope)) return false;
    if (this.desired.version >= MAX_SAFE_VERSION) return false;
    if (!isManagementDraftStatus({ version: 0, dirty, kind })) return false;
    this.desired = {
      version: this.desired.version + 1,
      dirty,
      kind: dirty ? kind : 'none',
    };
    this.confirmed = null;
    this.pending = { ...this.desired };
    this.notify();
    void this.pump();
    return true;
  }

  flush(): Promise<boolean> {
    if (this.cancelled) return Promise.resolve(false);
    if (this.isSynced) return Promise.resolve(true);
    if (!this.running && this.pending === null) this.pending = { ...this.desired };
    return new Promise((resolve) => {
      this.waiters.push(resolve);
      void this.pump();
    });
  }

  /**
   * 作用域（选择修订）推进后取消旧队列：不派发尚未发出的合并值，也不把在途
   * 回执写入已被替换的实例。调用方保留自己的表单正文，并在用户于新作用域
   * 重新编辑或明确受控协调时再建立新同步。
   */
  cancel(): void {
    if (this.cancelled) return;
    this.cancelled = true;
    this.pending = null;
    this.confirmed = null;
    const waiters = this.waiters;
    this.waiters = [];
    this.notify();
    for (const resolve of waiters) resolve(false);
  }

  private async pump(): Promise<void> {
    if (this.cancelled || this.running) return;
    this.running = true;
    try {
      while (!this.cancelled && this.pending !== null) {
        const request = this.pending;
        this.pending = null;
        try {
          const ack = await this.dispatch(request.version, request.dirty, request.kind);
          // 取消后旧作用域的在途回执不再写入本实例，也不影响新作用域的基线。
          if (this.cancelled) break;
          if (ack.version === request.version && ack.dirty === request.dirty) {
            this.confirmed = request;
          } else {
            this.confirmed = null;
            this.onFailure?.();
          }
        } catch {
          if (this.cancelled) break;
          this.confirmed = null;
          this.onFailure?.();
        }
      }
    } finally {
      this.running = false;
      const waiters = this.waiters;
      this.waiters = [];
      const synced = this.isSynced;
      if (!this.cancelled) this.notify();
      for (const resolve of waiters) resolve(synced);
    }
  }
}

export function consumeManagementDiscardReceipt(
  snapshot: ManagementDiscardReceipt | null,
  sync: ManagementDraftSync,
  clearDrafts: () => void,
  scope: ManagementDraftScope = sync.scope,
): boolean {
  const version = snapshot?.managementDiscardedVersion;
  if (
    version === null ||
    version === undefined ||
    !sync.consumeDiscardedVersion(version, scope)
  ) {
    return false;
  }
  clearDrafts();
  return true;
}
