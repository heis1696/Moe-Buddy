/**
 * 未知副作用命令锁。
 *
 * 侧作用命令（发送、关闭、撤回等）结果不明时，界面必须先保持锁定。普通快照事件
 * 只更新展示数据，不能证明未知命令已收口；读取也必须等业务队列屏障成功返回，
 * 不能在刷新开始时先清零再被旧缓存事件解锁。
 *
 * A 会话生命周期：
 * - 只有 `send` 未知属于发生它的会话；切到/新建另一会话不会解除旧会话的锁。
 * - 记忆、设置、存储、撤权等未知不是会话作用域，不能被新建会话或普通快照解除。
 * - 是否允许显式另开隔离会话由应用快照的 `sessionIsolationAllowed` 裁决；
 *   本锁只记录未知作用域，不按操作名或错误码推导隔离许可。
 * - IPC 丢回包不触发任何自动重发。
 *
 * B 伙伴注册表：
 * - 锁按“伙伴作用域 + 会话作用域”隔离；另一伙伴的同名局部会话不共享未知，
 *   A→B→A 切回原伙伴仍保留它自己的未知。
 * - 伙伴切换前的全局未知会阻止切换，锁本身不因此被清空或搬运到新伙伴。
 */
export type UnknownSource = 'ipc_reply_lost' | 'business_unknown';

interface Unconfirmed {
  operation: string;
  source: UnknownSource;
}

function sessionKey(buddyId: string, sessionId: string): string {
  return `${buddyId}\u0000${sessionId}`;
}

export class CommandUnconfirmedLock {
  /** 伙伴作用域内的全局未知（记忆、设置、存储、撤权等）；键为伙伴 ID。 */
  private readonly globalByBuddy = new Map<string, Unconfirmed>();
  private readonly bySession = new Map<string, Unconfirmed>();
  private currentBuddyId = '';
  private currentSessionId: string | null = null;
  private refreshing = false;

  get locked(): boolean {
    return this.currentGlobal !== null || this.currentUnknown !== null;
  }

  get source(): UnknownSource | null {
    return (this.currentGlobal ?? this.currentUnknown)?.source ?? null;
  }

  get operation(): string | null {
    return (this.currentGlobal ?? this.currentUnknown)?.operation ?? null;
  }

  get isRefreshing(): boolean {
    return this.refreshing;
  }

  get currentSession(): string | null {
    return this.currentSessionId;
  }

  get currentBuddy(): string {
    return this.currentBuddyId;
  }

  get hasGlobalUnknown(): boolean {
    return this.currentGlobal !== null;
  }

  get hasSessionUnknown(): boolean {
    return this.currentUnknown !== null;
  }

  /** 进入某个伙伴的当前可见会话；旧伙伴/旧会话的未知仍保留在映射中。 */
  enterScope(buddyId: string, sessionId: string | null): void {
    this.currentBuddyId = buddyId;
    this.currentSessionId = sessionId;
  }

  /** 切换伙伴作用域；不同伙伴的局部会话 ID 不共用未知条目。 */
  enterBuddy(buddyId: string): void {
    this.currentBuddyId = buddyId;
    this.currentSessionId = null;
  }

  /** 宿主签发/切换了当前可见会话；旧会话的未知仍保留在映射中。 */
  enterSession(sessionId: string | null): void {
    this.currentSessionId = sessionId;
  }

  /**
   * `send` 且已知会话 ID 时按 (伙伴, 会话) 隔离；其余一律记入当前伙伴的全局未知。
   * 未提供会话 ID 的发送（宿主尚未签发目录）也只能按全局未知保守处理。
   */
  markUnknown(
    operation: string,
    source: UnknownSource,
    sessionId: string | null = this.currentSessionId,
    buddyId: string = this.currentBuddyId,
  ): void {
    if (operation === 'send' && sessionId !== null) {
      this.bySession.set(sessionKey(buddyId, sessionId), { operation, source });
      return;
    }
    this.globalByBuddy.set(buddyId, { operation, source });
  }

  /** 开始一次状态重读；在读取返回前保持现有锁。 */
  beginRefresh(): void {
    this.refreshing = true;
  }

  /**
   * 普通状态重读只更新展示；没有对应的确定性命令回执时永不解除副作用锁。
   */
  finishRefresh(_readSucceeded: boolean, _hasSnapshot: boolean): void {
    if (!this.refreshing) return;
    this.refreshing = false;
  }

  /**
   * 命令自己的成功响应已经确定结果，可只清对应会话的锁。
   * 迟到/跨会话回包必须传入发生命令时的会话，避免误清另一会话；
   * 指定会话时不回退清理全局未知，只有无会话上下文的确认才清全局锁。
   * 未显式提供伙伴时使用当前伙伴作用域。
   */
  confirm(operation: string, sessionId: string | null = this.currentSessionId): void {
    this.confirmScoped(operation, this.currentBuddyId, sessionId);
  }

  /** 按发起命令时捕获的伙伴/会话作用域确认，不落到当前伙伴。 */
  confirmScoped(operation: string, buddyId: string, sessionId: string | null): void {
    if (sessionId !== null) {
      const key = sessionKey(buddyId, sessionId);
      const scoped = this.bySession.get(key);
      if (scoped?.operation === operation) this.bySession.delete(key);
      return;
    }
    if (this.globalByBuddy.get(buddyId)?.operation === operation) {
      this.globalByBuddy.delete(buddyId);
    }
  }

  /** 撤权后把原伙伴的全局未知迁到空作用域：清展示缓存不等于把未知当成已收口。 */
  migrateGlobalUnknown(fromBuddyId: string, toBuddyId: string): void {
    if (fromBuddyId === toBuddyId) return;
    const existing = this.globalByBuddy.get(fromBuddyId);
    if (existing === undefined) return;
    if (!this.globalByBuddy.has(toBuddyId)) {
      this.globalByBuddy.set(toBuddyId, existing);
    }
    this.globalByBuddy.delete(fromBuddyId);
  }

  /** 撤权或组件释放后清空全部作用域，避免旧伙伴的未知被重新采纳。 */
  reset(): void {
    this.globalByBuddy.clear();
    this.bySession.clear();
    this.currentBuddyId = '';
    this.currentSessionId = null;
    this.refreshing = false;
  }

  private get currentGlobal(): Unconfirmed | null {
    return this.globalByBuddy.get(this.currentBuddyId) ?? null;
  }

  private get currentUnknown(): Unconfirmed | null {
    if (this.currentSessionId === null) return null;
    return this.bySession.get(sessionKey(this.currentBuddyId, this.currentSessionId)) ?? null;
  }
}
