import { IPC_CONTRACT } from './contract';
import type {
  PrototypeSnapshot,
  SessionCatalogView,
  SessionExpectation,
  SessionStatus,
  SessionView,
} from './types';

/**
 * A 会话生命周期的纯前端边界：
 * - 只校验宿主已签发的只读目录，不构造 ID、归属或权限；
 * - 目录三元组与隔离许可是冻结协议的必填项，缺字段由快照解析整份拒绝；
 * - 草稿按会话有界保留，不因切换丢稿，也不把旧稿带入新会话。
 */

const SESSION_FIELDS: readonly string[] = IPC_CONTRACT.sessionFields;
const SESSION_SNAPSHOT_FIELDS: readonly string[] = IPC_CONTRACT.sessionSnapshotFields;
const SESSION_STATUSES: readonly string[] = IPC_CONTRACT.sessionStatuses;
const SESSION_ID_MAX_BYTES = IPC_CONTRACT.sessionLimits.idMaxBytes;
const SESSION_DIRECTORY_MAX = IPC_CONTRACT.sessionLimits.directoryMax;

/** 会话草稿条目上限与目录上限一致；超出时淘汰最早使用的会话草稿。 */
export const SESSION_DRAFT_LIMIT = SESSION_DIRECTORY_MAX;

/**
 * 单条草稿的本地保留上限（UTF-16 单元）。宿主命令载荷本身远小于此值；
 * 超长文本无法完成同步，切换会因同步失败而取消并保留当前输入，因此不另存副本。
 */
export const SESSION_DRAFT_MAX_CHARS = 8192;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function hasOwn(value: Record<string, unknown>, key: string): boolean {
  return Object.prototype.hasOwnProperty.call(value, key);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  return actual.length === expected.length && actual.every((key, index) => key === expected[index]);
}

function isSafeCount(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

/**
 * 冻结 ID 规则：1..64 字节、非空、可打印 ASCII（不含空白/控制字符/非 ASCII）。
 * 前端只用于拒绝宿主异常目录，不据此签发或猜测 ID。
 */
export function isSafeSessionId(value: unknown): value is string {
  if (typeof value !== 'string') return false;
  if (value.length === 0 || value.length > SESSION_ID_MAX_BYTES) return false;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    if (code < 0x21 || code > 0x7e) return false;
  }
  return true;
}

function isSessionStatus(value: unknown): value is SessionStatus {
  return typeof value === 'string' && SESSION_STATUSES.includes(value);
}

function parseSessions(value: unknown, revoked: boolean): SessionView[] | null {
  if (!Array.isArray(value) || value.length > SESSION_DIRECTORY_MAX) return null;
  if (revoked && value.length > 0) return null;
  const sessions: SessionView[] = [];
  const seen = new Set<string>();
  for (const item of value) {
    if (!isRecord(item) || !hasExactKeys(item, SESSION_FIELDS)) return null;
    if (!isSafeSessionId(item.id) || typeof item.label !== 'string') return null;
    if (typeof item.archived !== 'boolean' || !isSessionStatus(item.status)) return null;
    if (seen.has(item.id)) return null;
    seen.add(item.id);
    sessions.push({
      id: item.id,
      label: item.label,
      archived: item.archived,
      status: item.status,
    });
  }
  return sessions;
}

/**
 * 解析快照中的会话目录三元组。
 * - 三个字段是当前冻结协议的必填项；缺任一字段返回 `null`（整份快照拒绝）。
 * - `null` 也覆盖重复 ID、超限、撤权后仍带目录等非法状态。
 * - 对象：严格校验后的只读目录。
 */
export function parseSessionCatalog(
  source: Record<string, unknown>,
  revoked: boolean,
): SessionCatalogView | null {
  if (!SESSION_SNAPSHOT_FIELDS.every((field) => hasOwn(source, field))) return null;

  const activeSessionId = source.activeSessionId;
  if (activeSessionId !== null && !isSafeSessionId(activeSessionId)) return null;
  if (!isSafeCount(source.sessionsRevision)) return null;
  const sessions = parseSessions(source.sessions, revoked);
  if (sessions === null) return null;

  if (revoked) {
    if (activeSessionId !== null) return null;
    if (sessions.length > 0) return null;
    return { activeSessionId: null, sessionsRevision: source.sessionsRevision, sessions };
  }
  if (activeSessionId === null) {
    // 尚未签发会话时目录必须为空；不允许出现“有目录但无选择”的悬空状态。
    if (sessions.length > 0) return null;
    return { activeSessionId: null, sessionsRevision: source.sessionsRevision, sessions };
  }
  if (!sessions.some((session) => session.id === activeSessionId)) return null;
  return { activeSessionId, sessionsRevision: source.sessionsRevision, sessions };
}

/**
 * 读取已经通过严格解析的会话目录。空快照或字段缺失（防御性检查）返回 null，
 * 界面据此禁用生命周期入口；这里不把缺字段当作旧协议继续使用。
 */
export function sessionCatalogOf(snapshot: PrototypeSnapshot | null | undefined): SessionCatalogView | null {
  if (snapshot === null || snapshot === undefined) return null;
  const { activeSessionId, sessionsRevision, sessions } = snapshot;
  if (activeSessionId === undefined || sessionsRevision === undefined || sessions === undefined) {
    return null;
  }
  return { activeSessionId, sessionsRevision, sessions };
}

/**
 * 生命周期命令与草稿/发送命令共享的当前预期。目录未接线时使用显式的
 * null / 0，含义是“宿主尚未签发任何会话”，不是伪造一个会话。
 */
export function sessionExpectation(
  snapshot: PrototypeSnapshot | null | undefined,
): SessionExpectation {
  const catalog = sessionCatalogOf(snapshot);
  if (catalog === null) {
    return { expectedSessionId: null, expectedSessionsRevision: 0 };
  }
  return {
    expectedSessionId: catalog.activeSessionId,
    expectedSessionsRevision: catalog.sessionsRevision,
  };
}

export function activeSessionOf(
  snapshot: PrototypeSnapshot | null | undefined,
): SessionView | null {
  const catalog = sessionCatalogOf(snapshot);
  if (catalog === null || catalog.activeSessionId === null) return null;
  return catalog.sessions.find((session) => session.id === catalog.activeSessionId) ?? null;
}

/** 归档会话仍可显式选择查看，但不得发送或继续编辑草稿。 */
export function isArchivedActiveSession(snapshot: PrototypeSnapshot | null | undefined): boolean {
  return activeSessionOf(snapshot)?.archived === true;
}

/** 目录状态也参与发送门禁：目录声明发送中/未知时按禁止处理，不能乐观看正文状态。 */
export function sessionStatusBlocksSend(snapshot: PrototypeSnapshot | null | undefined): boolean {
  const status = activeSessionOf(snapshot)?.status;
  return status === 'sending' || status === 'unknown';
}

/**
 * 单个会话的本地草稿副本及其同步元数据：
 * - `revision` 是该正文对应的宿主草稿集合修订；
 * - `generation` 是写入这份正文时的本地编辑代际；
 * - `synced` 表示这份正文已由宿主接受（未同步正文在明确丢弃时保留）。
 */
export interface SessionDraftEntry {
  text: string;
  revision: number;
  generation: number;
  synced: boolean;
}

/**
 * 有界按会话草稿存储。切换时不搬运正文：只按目标会话取回自己的草稿；
 * 卸载/撤权由调用方 clear，不落盘、不导出。每份缓存记录同步修订、本地
 * 编辑代际与是否已同步，供逐会话消费丢弃水位。
 */
export class SessionDraftStore {
  private readonly drafts = new Map<string, SessionDraftEntry>();
  private readonly limit: number;

  constructor(limit: number = SESSION_DRAFT_LIMIT) {
    this.limit = limit > 0 ? limit : 1;
  }

  get size(): number {
    return this.drafts.size;
  }

  has(sessionId: string): boolean {
    return isSafeSessionId(sessionId) && this.drafts.has(sessionId);
  }

  remember(
    sessionId: string,
    text: string,
    revision: number = 0,
    generation: number = 0,
    synced: boolean = false,
  ): void {
    if (!isSafeSessionId(sessionId)) return;
    if (text.length > SESSION_DRAFT_MAX_CHARS) return;
    // 重新插入以刷新淘汰顺序。
    this.drafts.delete(sessionId);
    this.drafts.set(sessionId, { text, revision, generation, synced });
    while (this.drafts.size > this.limit) {
      const oldest = this.drafts.keys().next();
      if (oldest.done === true) break;
      this.drafts.delete(oldest.value);
    }
  }

  /** 只读查看缓存条目，不改变淘汰顺序。 */
  peek(sessionId: string): SessionDraftEntry | null {
    if (!isSafeSessionId(sessionId)) return null;
    return this.drafts.get(sessionId) ?? null;
  }

  recallEntry(sessionId: string): SessionDraftEntry | null {
    if (!isSafeSessionId(sessionId)) return null;
    const entry = this.drafts.get(sessionId);
    if (entry === undefined) return null;
    this.drafts.delete(sessionId);
    this.drafts.set(sessionId, entry);
    return entry;
  }

  recall(sessionId: string): string | null {
    return this.recallEntry(sessionId)?.text ?? null;
  }

  /**
   * 宿主确认这份正文后记录其权威修订。仅当缓存仍是同一份正文时更新；
   * 用户在途编辑遗留的更新正文不能被旧回包标记成已同步。
   */
  markSynced(sessionId: string, text: string, revision: number): void {
    const entry = this.peek(sessionId);
    if (entry === null || entry.text !== text) return;
    entry.revision = revision;
    entry.synced = true;
  }

  forget(sessionId: string): void {
    this.drafts.delete(sessionId);
  }

  clear(): void {
    this.drafts.clear();
  }
}
