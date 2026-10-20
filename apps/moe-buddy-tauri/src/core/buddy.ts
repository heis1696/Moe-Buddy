import { IPC_CONTRACT } from './contract';
import {
  SESSION_DRAFT_LIMIT,
  SessionDraftStore,
  isSafeSessionId,
  type SessionDraftEntry,
} from './session';
import type {
  BuddyCatalogView,
  BuddyCommitKind,
  BuddyCommitReceipt,
  BuddyExpectation,
  BuddyView,
  PrototypeSnapshot,
} from './types';

/**
 * B 伙伴注册表的前端边界：
 * - 只严格解析宿主签发的只读注册表，不构造实例 ID、归属或权限；
 * - 每个伙伴的缓存、草稿与水位按伙伴作用域隔离，聊天再按会话 ID 隔离；
 * - 显示名、人设与素材都不是身份，换皮不改变实例 ID。
 */

const BUDDY_FIELDS: readonly string[] = IPC_CONTRACT.buddyFields;
const BUDDY_SNAPSHOT_FIELDS: readonly string[] = IPC_CONTRACT.buddySnapshotFields;
const BUDDY_COMMIT_FIELDS: readonly string[] = IPC_CONTRACT.buddyCommitFields;
const BUDDY_COMMIT_KINDS: readonly string[] = IPC_CONTRACT.buddyCommitKinds;
const BUDDY_ID_MAX_BYTES = IPC_CONTRACT.buddyLimits.idMaxBytes;
const BUDDY_LABEL_MAX_SCALARS = IPC_CONTRACT.buddyLimits.labelMaxScalars;
/** 首批注册表上限（8 个伙伴）；由受信服务裁决，前端只用于禁用新建入口。 */
export const BUDDY_REGISTRY_MAX = IPC_CONTRACT.buddyLimits.registryMax;
/** 显示名上限（Unicode 标量），用于界面提示与本地校验。 */
export const BUDDY_LABEL_MAX = BUDDY_LABEL_MAX_SCALARS;

/** 新建/改名提交的人设上限（UTF-16 单元）。超出时本地拒绝，不截断也不静默丢弃。 */
export const BUDDY_PERSONA_MAX_CHARS = IPC_CONTRACT.buddyLimits.personaMaxChars;

/** 无活动伙伴时上下文使用的空串哨兵；它不是合法的伙伴 ID。 */
export const NO_ACTIVE_BUDDY = '';

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

/** 修订是正的 JavaScript 安全整数；耗尽或越界都明确拒绝。 */
export function isPositiveRevision(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 1;
}

/** 管理状态版本是非负 JavaScript 安全整数；0 是合法初始版本。 */
function isManagementVersion(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

/**
 * 冻结伙伴 ID 规则与 Rust `BuddyId::new` 对齐：
 * - 1..64 字节，首字符为 ASCII 字母/数字；
 * - 其余字符只允许 ASCII 字母、数字、点、下划线或连字符；
 * - `.` 与 `..` 保留给路径语义，明确拒绝。
 * 前端只用于拒绝宿主异常注册表，不据此签发或猜测 ID。
 */
export function isSafeBuddyId(value: unknown): value is string {
  if (typeof value !== 'string') return false;
  if (value.length === 0 || value.length > BUDDY_ID_MAX_BYTES) return false;
  if (value === '.' || value === '..') return false;
  for (let index = 0; index < value.length; index += 1) {
    const code = value.charCodeAt(index);
    const alphaNumeric =
      (code >= 0x30 && code <= 0x39) ||
      (code >= 0x41 && code <= 0x5a) ||
      (code >= 0x61 && code <= 0x7a);
    if (index === 0) {
      if (!alphaNumeric) return false;
      continue;
    }
    if (!alphaNumeric && code !== 0x2d && code !== 0x2e && code !== 0x5f) return false;
  }
  return true;
}

/**
 * 严格解析宿主签发的无正文 buddyCommit 回执：
 * - 字段集合必须恰好等于共享夹具；null 合法（启动、撤权或尚无档案动作）；
 * - kind 只允许冻结的五种动作；
 * - 两个伙伴 ID、两个修订和管理版本都必须是合法安全值，`toRevision` 恰好
 *   等于 `fromRevision + 1`；
 * - 撤权快照不得携带回执。
 *
 * 返回 `undefined` 表示结构非法（整份快照拒绝），`null` 表示明确的“无回执”。
 */
export function parseBuddyCommit(
  value: unknown,
  revoked: boolean,
): BuddyCommitReceipt | null | undefined {
  if (value === null) return null;
  if (revoked) return undefined;
  if (!isRecord(value) || !hasExactKeys(value, BUDDY_COMMIT_FIELDS)) return undefined;
  if (!BUDDY_COMMIT_KINDS.includes(value.kind as never)) return undefined;
  if (!isSafeBuddyId(value.fromBuddyId) || !isSafeBuddyId(value.toBuddyId)) return undefined;
  if (!isPositiveRevision(value.fromRevision) || !isPositiveRevision(value.toRevision)) {
    return undefined;
  }
  if (value.toRevision !== value.fromRevision + 1) return undefined;
  if (!isManagementVersion(value.managementVersion)) return undefined;
  return {
    kind: value.kind as BuddyCommitKind,
    fromBuddyId: value.fromBuddyId,
    fromRevision: value.fromRevision,
    toBuddyId: value.toBuddyId,
    toRevision: value.toRevision,
    managementVersion: value.managementVersion,
  };
}

/**
 * 精确匹配发起动作时的完整证据：动作、发起伙伴、发起修订与管理版本。
 * 回执的新 context 由调用方再与本地代际/编辑代际一起复核，不能只凭这里匹配
 * 就清掉更新编辑。
 */
export function buddyCommitMatchesIntent(
  intent: {
    kind: BuddyCommitKind;
    fromBuddyId: string;
    fromRevision: number;
    managementVersion: number;
  },
  receipt: BuddyCommitReceipt | null,
): boolean {
  if (receipt === null) return false;
  return (
    receipt.kind === intent.kind &&
    receipt.fromBuddyId === intent.fromBuddyId &&
    receipt.fromRevision === intent.fromRevision &&
    receipt.managementVersion === intent.managementVersion &&
    receipt.toRevision === receipt.fromRevision + 1
  );
}

function scalarCount(text: string): number {
  return Array.from(text).length;
}

function hasControlCharacter(text: string): boolean {
  for (const character of text) {
    const code = character.codePointAt(0) ?? 0;
    if (code < 0x20 || (code >= 0x7f && code <= 0x9f)) return true;
  }
  return false;
}

/** 显示名最多 64 个 Unicode 标量且不允许控制字符；空名允许（展示回退到 ID）。 */
export function isSafeBuddyLabel(value: unknown): value is string {
  if (typeof value !== 'string') return false;
  if (scalarCount(value) > BUDDY_LABEL_MAX_SCALARS) return false;
  return !hasControlCharacter(value);
}

/** 创建/改名时本地名称校验：不允许空名、超长或控制字符，不静默裁剪。 */
export function buddyLabelProblem(label: string): string | null {
  if (label.trim().length === 0) return '伙伴名称不能为空。';
  if (!isSafeBuddyLabel(label)) {
    return `伙伴名称不能超过 ${BUDDY_LABEL_MAX_SCALARS} 个 Unicode 字符，也不能包含控制字符。`;
  }
  return null;
}

/**
 * 人设与 Rust `validate_persona` 边界一致：非空白、最多 4096 个 Unicode 标量。
 * 为保留 D/A 已接受的多行人设，允许 LF/CR/TAB，其余控制字符拒绝。
 */
export function buddyPersonaProblem(persona: string): string | null {
  if (persona.trim().length === 0) {
    return '人设不能为空。';
  }
  if (scalarCount(persona) > BUDDY_PERSONA_MAX_CHARS) {
    return `人设不能超过 ${BUDDY_PERSONA_MAX_CHARS} 个 Unicode 字符。`;
  }
  if (hasControlCharacter(persona.replace(/[\n\r\t]/g, ''))) {
    return '人设不能包含换行、回车、制表以外的控制字符。';
  }
  return null;
}

/** 快照中当前档案人设的只读校验；不返回任何其他伙伴正文。 */
export function isSafeBuddyPersona(value: unknown): value is string {
  return typeof value === 'string' && buddyPersonaProblem(value) === null;
}

function parseBuddies(value: unknown, revoked: boolean): BuddyView[] | null {
  if (!Array.isArray(value) || value.length > BUDDY_REGISTRY_MAX) return null;
  if (revoked && value.length > 0) return null;
  const buddies: BuddyView[] = [];
  const seen = new Set<string>();
  for (const item of value) {
    if (!isRecord(item) || !hasExactKeys(item, BUDDY_FIELDS)) return null;
    if (!isSafeBuddyId(item.id) || !isSafeBuddyLabel(item.label)) return null;
    if (
      !isPositiveRevision(item.profileRevision) ||
      !isPositiveRevision(item.personaRevision) ||
      !isPositiveRevision(item.modelRevision)
    ) {
      return null;
    }
    if (seen.has(item.id)) return null;
    seen.add(item.id);
    buddies.push({
      id: item.id,
      label: item.label,
      profileRevision: item.profileRevision,
      personaRevision: item.personaRevision,
      modelRevision: item.modelRevision,
    });
  }
  return buddies;
}

/**
 * 解析快照中的伙伴注册表三元组。
 * - 三个字段是当前冻结协议的必填项；缺任一字段返回 `null`（整份快照拒绝）。
 * - `null` 也覆盖重复 ID、超过 8 个伙伴、非法修订、撤权后仍带目录等非法状态。
 * - 撤权后活动 ID 必须为 null 且列表为空；没有活动 ID 时列表也必须为空。
 */
export function parseBuddyCatalog(
  source: Record<string, unknown>,
  revoked: boolean,
): BuddyCatalogView | null {
  if (!BUDDY_SNAPSHOT_FIELDS.every((field) => hasOwn(source, field))) return null;

  const activeBuddyId = source.activeBuddyId;
  if (activeBuddyId !== null && !isSafeBuddyId(activeBuddyId)) return null;
  if (!isPositiveRevision(source.buddiesRevision)) return null;
  const buddies = parseBuddies(source.buddies, revoked);
  if (buddies === null) return null;

  if (revoked) {
    if (activeBuddyId !== null || buddies.length > 0) return null;
    return { activeBuddyId: null, buddiesRevision: source.buddiesRevision, buddies };
  }
  if (activeBuddyId === null) {
    // 尚未签发伙伴时列表必须为空；不允许出现“有档案但无选择”的悬空状态。
    if (buddies.length > 0) return null;
    return { activeBuddyId: null, buddiesRevision: source.buddiesRevision, buddies };
  }
  if (!buddies.some((buddy) => buddy.id === activeBuddyId)) return null;
  return { activeBuddyId, buddiesRevision: source.buddiesRevision, buddies };
}

/** 读取已经严格解析的注册表；空快照或字段缺失（防御性检查）返回 null。 */
export function buddyCatalogOf(
  snapshot: PrototypeSnapshot | null | undefined,
): BuddyCatalogView | null {
  if (snapshot === null || snapshot === undefined) return null;
  const { activeBuddyId, buddiesRevision, buddies } = snapshot;
  if (activeBuddyId === undefined || buddiesRevision === undefined || buddies === undefined) {
    return null;
  }
  return { activeBuddyId, buddiesRevision, buddies };
}

/**
 * 每个业务命令发起时捕获的伙伴上下文。目录未接线时使用显式的空串 / 0 哨兵，
 * 含义是“宿主尚未签发任何伙伴”，不是伪造一个实例。
 */
export function buddyExpectationOf(
  snapshot: PrototypeSnapshot | null | undefined,
): BuddyExpectation {
  const catalog = buddyCatalogOf(snapshot);
  if (catalog === null) {
    return { expectedBuddyId: NO_ACTIVE_BUDDY, expectedBuddiesRevision: 0 };
  }
  return {
    expectedBuddyId: catalog.activeBuddyId ?? NO_ACTIVE_BUDDY,
    expectedBuddiesRevision: catalog.buddiesRevision,
  };
}

export function activeBuddyOf(
  snapshot: PrototypeSnapshot | null | undefined,
): BuddyView | null {
  const catalog = buddyCatalogOf(snapshot);
  if (catalog === null || catalog.activeBuddyId === null) return null;
  return catalog.buddies.find((buddy) => buddy.id === catalog.activeBuddyId) ?? null;
}

/**
 * 完整伙伴作用域比较：活动 ID 与选择修订共同标识一次选择。
 * 只比较 ID 会把“同一伙伴的新选择修订”误当成旧作用域，从而给旧编辑
 * 换上新的修订；两个字段都必须一致才算同一作用域。
 */
export function buddyExpectationMatches(
  previous: BuddyExpectation,
  next: BuddyExpectation,
): boolean {
  return (
    previous.expectedBuddyId === next.expectedBuddyId &&
    previous.expectedBuddiesRevision === next.expectedBuddiesRevision
  );
}

/**
 * 判断伙伴作用域栅栏是否变化：活动 ID 或选择修订任一变化都算变化。
 * A→B→A 不能因为 ID 相同就回到旧修订，迟到的回包与缓存按两者共同隔离。
 */
export function buddyScopeChanged(
  previous: BuddyExpectation,
  next: BuddyExpectation,
): boolean {
  return !buddyExpectationMatches(previous, next);
}

/**
 * 草稿缓存只接受宿主签发的真实伙伴 ID；空串只是“尚无伙伴”的上下文哨兵，
 * 不能成为可用的业务缓存作用域。
 */
function isBuddyScopeId(value: unknown): value is string {
  return isSafeBuddyId(value);
}

/**
 * 按伙伴隔离的会话草稿缓存：同一局部会话 ID 在不同伙伴下不共享正文。
 * 每个伙伴内部沿用 A 的按会话有界淘汰，伙伴作用域总数也由注册表上限约束。
 */
export class BuddySessionDrafts {
  private readonly scopes = new Map<string, SessionDraftStore>();
  private readonly perBuddyLimit: number;
  private readonly maxBuddies: number;

  constructor(perBuddyLimit?: number, maxBuddies: number = BUDDY_REGISTRY_MAX) {
    this.perBuddyLimit = perBuddyLimit ?? SESSION_DRAFT_LIMIT;
    this.maxBuddies = maxBuddies > 0 ? maxBuddies : 1;
  }

  get scopeCount(): number {
    return this.scopes.size;
  }

  /** 某个伙伴作用域当前缓存的草稿条数（测试与界面诊断用，不暴露正文）。 */
  count(buddyId: string): number {
    return this.scopes.get(buddyId)?.size ?? 0;
  }

  private store(buddyId: string, create: boolean): SessionDraftStore | null {
    if (!isBuddyScopeId(buddyId)) return null;
    const existing = this.scopes.get(buddyId);
    if (existing !== undefined) {
      // 重新插入以刷新作用域淘汰顺序。
      this.scopes.delete(buddyId);
      this.scopes.set(buddyId, existing);
      return existing;
    }
    if (!create) return null;
    const store = new SessionDraftStore(this.perBuddyLimit);
    this.scopes.set(buddyId, store);
    while (this.scopes.size > this.maxBuddies) {
      const oldest = this.scopes.keys().next();
      if (oldest.done === true) break;
      this.scopes.delete(oldest.value);
    }
    return store;
  }

  remember(
    buddyId: string,
    sessionId: string,
    text: string,
    revision: number = 0,
    generation: number = 0,
    synced: boolean = false,
  ): void {
    this.store(buddyId, true)?.remember(sessionId, text, revision, generation, synced);
  }

  peek(buddyId: string, sessionId: string): SessionDraftEntry | null {
    return this.store(buddyId, false)?.peek(sessionId) ?? null;
  }

  recallEntry(buddyId: string, sessionId: string): SessionDraftEntry | null {
    return this.store(buddyId, false)?.recallEntry(sessionId) ?? null;
  }

  markSynced(buddyId: string, sessionId: string, text: string, revision: number): void {
    this.store(buddyId, false)?.markSynced(sessionId, text, revision);
  }

  forget(buddyId: string, sessionId: string): void {
    this.store(buddyId, false)?.forget(sessionId);
  }

  /** 只保留当前注册表仍列出的伙伴作用域；撤权时整体 clear。 */
  prune(keepBuddyIds: readonly string[]): void {
    const keep = new Set(keepBuddyIds);
    for (const key of [...this.scopes.keys()]) {
      if (!keep.has(key)) this.scopes.delete(key);
    }
  }

  clearBuddy(buddyId: string): void {
    this.scopes.delete(buddyId);
  }

  clear(): void {
    this.scopes.clear();
  }
}

/**
 * 按伙伴隔离的聊天草稿丢弃水位。其他伙伴或会话的编辑不会让本伙伴的旧回执
 * 失效，漏收中间丢弃事件时仍按该伙伴该会话的最新水位补清。
 */
export class BuddyDraftDiscardWatermarks {
  private readonly perBuddy = new Map<string, Map<string, number>>();
  private readonly maxBuddies: number;
  private readonly maxSessions: number;

  constructor(maxBuddies: number = BUDDY_REGISTRY_MAX, maxSessions: number = 64) {
    this.maxBuddies = maxBuddies > 0 ? maxBuddies : 1;
    this.maxSessions = maxSessions > 0 ? maxSessions : 1;
  }

  get(buddyId: string, sessionId: string): number {
    if (!isBuddyScopeId(buddyId) || !isSafeSessionId(sessionId)) return 0;
    return this.perBuddy.get(buddyId)?.get(sessionId) ?? 0;
  }

  /** 水位单调；重复/较旧水位不倒退。 */
  raise(buddyId: string, sessionId: string, watermark: number): number {
    if (!isBuddyScopeId(buddyId) || !isSafeSessionId(sessionId)) return 0;
    if (!Number.isSafeInteger(watermark) || watermark < 0) return this.get(buddyId, sessionId);
    let sessions = this.perBuddy.get(buddyId);
    if (sessions === undefined) {
      sessions = new Map<string, number>();
      this.perBuddy.set(buddyId, sessions);
      while (this.perBuddy.size > this.maxBuddies) {
        const oldest = this.perBuddy.keys().next();
        if (oldest.done === true) break;
        this.perBuddy.delete(oldest.value);
      }
    }
    const current = sessions.get(sessionId) ?? 0;
    const next = watermark > current ? watermark : current;
    sessions.set(sessionId, next);
    while (sessions.size > this.maxSessions) {
      const oldest = sessions.keys().next();
      if (oldest.done === true) break;
      sessions.delete(oldest.value);
    }
    return next;
  }

  prune(keepBuddyIds: readonly string[]): void {
    const keep = new Set(keepBuddyIds);
    for (const key of [...this.perBuddy.keys()]) {
      if (!keep.has(key)) this.perBuddy.delete(key);
    }
  }

  clearBuddy(buddyId: string): void {
    this.perBuddy.delete(buddyId);
  }

  clear(): void {
    this.perBuddy.clear();
  }
}
