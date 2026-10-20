import type {
  ChatDraftSessionView,
  ChatDraftView,
  ChatMessage,
  DiagnosticView,
  ManagementDraftView,
  ManagementExitView,
  MemoryView,
  ModelSettingsView,
  PresetView,
  PrototypeError,
  PrototypeSnapshot,
  SnapshotResponse,
} from './types';
import { IPC_CONTRACT } from './contract';
import { isSafeBuddyPersona, parseBuddyCatalog, parseBuddyCommit } from './buddy';
import { isSafeSessionId, parseSessionCatalog } from './session';

export function isRecord(value: unknown): value is Record<string, unknown> {
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

/** 必填字段齐全，且不允许未知字段；A/B 三元组都由各自解析器再单独必填校验。 */
function hasFieldsWithin(
  value: Record<string, unknown>,
  required: readonly string[],
  allowed: readonly string[],
): boolean {
  if (!required.every((key) => hasOwn(value, key))) return false;
  return Object.keys(value).every((key) => allowed.includes(key));
}

function isSafeCount(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0;
}

/**
 * D 快照字段 + A 会话目录三元组 + B 伙伴注册表三元组都是当前冻结协议的必填字段。
 * 缺任一字段、出现未知字段或任何半套旧快照都整份拒绝，不做双版本兼容。
 */
const SNAPSHOT_REQUIRED_FIELDS: readonly string[] = [
  ...new Set([
    ...IPC_CONTRACT.snapshotFields,
    ...IPC_CONTRACT.sessionSnapshotFields,
    ...IPC_CONTRACT.buddySnapshotFields,
  ]),
];

function parseError(value: unknown): PrototypeError | null | undefined {
  if (value === null) return null;
  if (!isRecord(value) || !hasExactKeys(value, IPC_CONTRACT.errorFields)) return undefined;
  if (typeof value.code !== 'string' || typeof value.message !== 'string') return undefined;
  if (!IPC_CONTRACT.dispositions.includes(value.disposition as never)) return undefined;
  return { code: value.code, message: value.message, disposition: value.disposition as PrototypeError['disposition'] };
}

/** `chat.failure` 只接受冻结契约里的可选字符串；不接受对象、数组等其他结构。 */
function parseFailure(value: unknown): string | null | undefined {
  if (value === null) return null;
  if (typeof value !== 'string') return undefined;
  return value;
}

function parseMessages(value: unknown): ChatMessage[] | null {
  if (!Array.isArray(value)) return null;
  const messages: ChatMessage[] = [];
  for (const item of value) {
    if (!isRecord(item) || !hasExactKeys(item, ['role', 'text']) || typeof item.role !== 'string' || typeof item.text !== 'string') {
      return null;
    }
    messages.push({ role: item.role, text: item.text });
  }
  return messages;
}

function parsePresets(value: unknown): PresetView[] | null {
  if (!Array.isArray(value)) return null;
  const presets: PresetView[] = [];
  for (const item of value) {
    if (!isRecord(item) || !hasExactKeys(item, IPC_CONTRACT.presetFields)) return null;
    if (
      typeof item.id !== 'string' ||
      typeof item.label !== 'string' ||
      typeof item.provider !== 'string' ||
      typeof item.model !== 'string' ||
      typeof item.endpoint !== 'string'
    ) {
      return null;
    }
    presets.push({
      id: item.id,
      label: item.label,
      provider: item.provider,
      model: item.model,
      endpoint: item.endpoint,
    });
  }
  return presets;
}

function parseMemories(value: unknown): MemoryView[] | null {
  if (!Array.isArray(value)) return null;
  const memories: MemoryView[] = [];
  for (const item of value) {
    if (!isRecord(item) || !hasExactKeys(item, IPC_CONTRACT.memoryFields)) return null;
    if (typeof item.id !== 'string' || !isSafeCount(item.revision) || typeof item.text !== 'string') {
      return null;
    }
    memories.push({ id: item.id, revision: item.revision, text: item.text });
  }
  return memories;
}

function parseDiagnostics(value: unknown): DiagnosticView[] | null {
  if (!Array.isArray(value)) return null;
  const diagnostics: DiagnosticView[] = [];
  for (const item of value) {
    if (!isRecord(item) || !hasExactKeys(item, IPC_CONTRACT.diagnosticFields)) return null;
    if (
      !isSafeCount(item.sequence) ||
      typeof item.run !== 'string' ||
      typeof item.event !== 'string' ||
      typeof item.status !== 'string'
    ) {
      return null;
    }
    diagnostics.push({
      sequence: item.sequence,
      run: item.run,
      event: item.event,
      status: item.status,
    });
  }
  return diagnostics;
}

/**
 * 快照里的模型配置只允许非凭据字段。这里用精确键校验，
 * 任何额外字段（尤其是 `apiKey`）都会让整份快照被拒绝，而不是被静默投影。
 */
function parseModelSettings(value: unknown): ModelSettingsView | null | undefined {
  if (value === null) return null;
  if (!isRecord(value) || !hasExactKeys(value, IPC_CONTRACT.modelSettingsFields)) return undefined;
  if (
    typeof value.endpoint !== 'string' ||
    typeof value.model !== 'string' ||
    typeof value.persona !== 'string' ||
    typeof value.allowTransmission !== 'boolean' ||
    typeof value.hasApiKey !== 'boolean'
  ) {
    return undefined;
  }
  return {
    endpoint: value.endpoint,
    model: value.model,
    persona: value.persona,
    allowTransmission: value.allowTransmission,
    hasApiKey: value.hasApiKey,
  };
}

function parseManagementDraft(value: unknown): ManagementDraftView | null {
  if (!isRecord(value) || !hasExactKeys(value, IPC_CONTRACT.managementDraftFields)) return null;
  if (!isSafeCount(value.version) || typeof value.dirty !== 'boolean') return null;
  if (!IPC_CONTRACT.managementDraftKinds.includes(value.kind as never)) return null;
  // clean 只能是 none；dirty 必须是创建/改名/一般编辑之一。
  if (value.dirty) {
    if (value.kind === 'none') return null;
  } else if (value.kind !== 'none') {
    return null;
  }
  return {
    version: value.version,
    dirty: value.dirty,
    kind: value.kind as ManagementDraftView['kind'],
  };
}

function parseManagementExit(value: unknown): ManagementExitView | null | undefined {
  if (value === null) return null;
  if (!isRecord(value) || !hasExactKeys(value, IPC_CONTRACT.managementExitFields)) return undefined;
  if (
    !isSafeCount(value.version) ||
    (value.intent !== 'close_chat' && value.intent !== 'quit_application')
  ) {
    return undefined;
  }
  return { version: value.version, intent: value.intent };
}

/**
 * 权威聊天草稿集合的严格解析：
 * - `revision`、`discard`、`sessions` 全部必填；缺少或出现未知字段均拒绝；
 * - `discard` 为 null 或 `{ revision, sessionIds }`，会话列表非空、至多目录上限、
 *   去重且是宿主签发的安全 ID，任何正文都不接受；
 * - `sessions` 至多目录上限，每项只含 `{ id, revision, discardedRevision }`，
 *   修订为安全整数且丢弃水位不得晚于该会话最近变更修订；撤权后必须为空。
 */
function parseChatDraftSessions(value: unknown): ChatDraftSessionView[] | null {
  if (!Array.isArray(value) || value.length > IPC_CONTRACT.sessionLimits.directoryMax) return null;
  const sessions: ChatDraftSessionView[] = [];
  const seen = new Set<string>();
  for (const item of value) {
    if (!isRecord(item) || !hasExactKeys(item, IPC_CONTRACT.chatDraftSessionFields)) return null;
    if (!isSafeSessionId(item.id) || seen.has(item.id)) return null;
    if (!isSafeCount(item.revision)) return null;
    const discarded = item.discardedRevision;
    if (discarded !== null && (!isSafeCount(discarded) || discarded > item.revision)) return null;
    seen.add(item.id);
    sessions.push({ id: item.id, revision: item.revision, discardedRevision: discarded });
  }
  return sessions;
}

function parseChatDraft(value: unknown): ChatDraftView | null {
  if (
    !isRecord(value) ||
    !hasExactKeys(value, IPC_CONTRACT.chatDraftFields)
  ) {
    return null;
  }
  if (!isSafeCount(value.revision)) return null;
  const sessions = parseChatDraftSessions(value.sessions);
  if (sessions === null || sessions.some((session) => session.revision > (value.revision as number))) return null;
  const discard = value.discard;
  if (discard === null) return { revision: value.revision, discard: null, sessions };
  if (!isRecord(discard) || !hasExactKeys(discard, IPC_CONTRACT.chatDraftDiscardFields)) return null;
  if (!isSafeCount(discard.revision) || discard.revision > value.revision) return null;
  if (
    !Array.isArray(discard.sessionIds) ||
    discard.sessionIds.length === 0 ||
    discard.sessionIds.length > IPC_CONTRACT.sessionLimits.directoryMax
  ) {
    return null;
  }
  const seen = new Set<string>();
  const sessionIds: string[] = [];
  for (const id of discard.sessionIds) {
    if (!isSafeSessionId(id) || seen.has(id)) return null;
    seen.add(id);
    sessionIds.push(id);
  }
  return {
    revision: value.revision,
    discard: { revision: discard.revision, sessionIds },
    sessions,
  };
}

export function parseSnapshot(value: unknown): PrototypeSnapshot | null {
  if (
    !isRecord(value) ||
    !hasFieldsWithin(
      value,
      IPC_CONTRACT.snapshotFields,
      SNAPSHOT_REQUIRED_FIELDS,
    )
  ) {
    return null;
  }
  if (!isSafeCount(value.revision) || !isSafeCount(value.presentationRevision)) return null;
  if (typeof value.mode !== 'string' || typeof value.revoked !== 'boolean') return null;
  if (typeof value.closeConfirmation !== 'boolean') return null;
  if (value.closeIntent !== null && value.closeIntent !== 'close_chat' && value.closeIntent !== 'quit_application') return null;
  if (value.closeConfirmation !== (value.closeIntent !== null)) return null;
  const managementDraft = parseManagementDraft(value.managementDraft);
  const managementExit = parseManagementExit(value.managementExit);
  if (
    managementDraft === null ||
    managementExit === undefined ||
    (managementExit !== null && managementExit.version > managementDraft.version)
  ) {
    return null;
  }
  if (value.managementDiscardedVersion !== null && !isSafeCount(value.managementDiscardedVersion)) {
    return null;
  }
  const chatDraft = parseChatDraft(value.chatDraft);
  // 撤权后目录与草稿回执/会话水位都必须为空；非空回执或水位不得在撤权快照中
  // 复活旧会话缓存。
  if (
    chatDraft === null ||
    (value.revoked && (chatDraft.discard !== null || chatDraft.sessions.length > 0))
  ) {
    return null;
  }
  if (!isRecord(value.chat) || !hasExactKeys(value.chat, ['draft', 'messages', 'status', 'failure'])) return null;
  if (!hasOwn(value.chat, 'failure')) return null;

  const draft = value.chat.draft;
  const status = value.chat.status;
  const messages = parseMessages(value.chat.messages);
  const failure = parseFailure(value.chat.failure);
  if (typeof draft !== 'string' || typeof status !== 'string') return null;
  if (messages === null || failure === undefined) return null;

  if (typeof value.activePreset !== 'string') return null;
  if (typeof value.externalTransmissionAllowed !== 'boolean') return null;
  if (typeof value.persistenceEnabled !== 'boolean') return null;
  if (typeof value.sessionIsolationAllowed !== 'boolean') return null;
  if (value.notice !== null && typeof value.notice !== 'string') return null;

  const presets = parsePresets(value.presets);
  const memories = parseMemories(value.memories);
  const diagnostics = parseDiagnostics(value.diagnostics);
  const modelSettings = parseModelSettings(value.modelSettings);
  if (presets === null || memories === null || diagnostics === null || modelSettings === undefined) return null;

  // A 会话目录：三元组与隔离许可都是必填项；任何缺失或非法都整份拒绝。
  const sessionCatalog = parseSessionCatalog(value, value.revoked);
  if (sessionCatalog === null) return null;
  // B 伙伴注册表：三元组同样是必填项，缺字段或非法注册表整份拒绝。
  const buddyCatalog = parseBuddyCatalog(value, value.revoked);
  if (buddyCatalog === null) return null;
  // B 档案提交回执：必填字段，但值可为 null；结构非法或撤权后残留都整份拒绝。
  const buddyCommit = parseBuddyCommit(value.buddyCommit, value.revoked);
  if (buddyCommit === undefined) return null;
  // 当前受信档案人设：有活动伙伴时必须是合法人设；无活动伙伴或撤权时为 null。
  const activeBuddyPersona = value.activeBuddyPersona;
  if (buddyCatalog.activeBuddyId === null) {
    if (activeBuddyPersona !== null) return null;
  } else if (!isSafeBuddyPersona(activeBuddyPersona)) {
    return null;
  }
  // 隔离许可只在存在当前会话且未撤权时才有意义；矛盾的快照不能被当成许可。
  if (value.sessionIsolationAllowed === true) {
    if (value.revoked || sessionCatalog.activeSessionId === null) return null;
  }

  const snapshot: PrototypeSnapshot = {
    revision: value.revision,
    presentationRevision: value.presentationRevision,
    mode: value.mode,
    revoked: value.revoked,
    chat: { draft, messages, status, failure },
    closeConfirmation: value.closeConfirmation,
    closeIntent: value.closeIntent,
    managementDraft,
    managementExit,
    managementDiscardedVersion: value.managementDiscardedVersion,
    chatDraft,
    activePreset: value.activePreset,
    externalTransmissionAllowed: value.externalTransmissionAllowed,
    presets,
    memories,
    diagnostics,
    notice: value.notice,
    modelSettings,
    persistenceEnabled: value.persistenceEnabled,
    activeBuddyId: buddyCatalog.activeBuddyId,
    buddiesRevision: buddyCatalog.buddiesRevision,
    buddies: buddyCatalog.buddies,
    activeBuddyPersona,
    buddyCommit,
    activeSessionId: sessionCatalog.activeSessionId,
    sessionsRevision: sessionCatalog.sessionsRevision,
    sessions: sessionCatalog.sessions,
    sessionIsolationAllowed: value.sessionIsolationAllowed,
  };
  return snapshot;
}

/**
 * 解析 invoke 返回值或事件负载。
 * 冻结契约为 `{ snapshot, error }`：两个字段都必须存在，snapshot 为 null 时只能伴随错误；
 * 不保留裸 snapshot 或 `{ snapshot: null, error: null }` 之类兼容分支。
 */
export function parseSnapshotResponse(payload: unknown): SnapshotResponse | null {
  if (!isRecord(payload) || !hasExactKeys(payload, ['snapshot', 'error'])) return null;
  if (!hasOwn(payload, 'snapshot') || !hasOwn(payload, 'error')) return null;

  const error = parseError(payload.error);
  if (error === undefined) return null;

  if (payload.snapshot === null) {
    if (error === null) return null;
    return { snapshot: null, error };
  }

  const snapshot = parseSnapshot(payload.snapshot);
  if (snapshot === null) return null;
  return { snapshot, error };
}

export type SnapshotUpdate =
  | { kind: 'none' }
  | { kind: 'success'; snapshot: PrototypeSnapshot; applied: boolean }
  | {
      kind: 'failure';
      error: PrototypeError;
      /** 已经被应用的实际快照；stale 或来源拒绝时为 null。 */
      snapshot: PrototypeSnapshot | null;
      /** 错误携带的旧 revision 快照不得覆盖较新的成功状态。 */
      stale: boolean;
    };

/**
 * revision 只增不减。请求与事件可能乱序到达，旧快照必须被丢弃。
 * 错误响应同样带 revision：旧 revision 的错误不能覆盖较新的成功状态，也不能把连接状态改成成功。
 * 初始拉取也走同一个入口，因此“事件先到、请求后到”不会回退界面。
 */
export class RevisionOrderedSnapshots {
  private latest: PrototypeSnapshot | null = null;
  private latestPresentationRevision = -1;

  get current(): PrototypeSnapshot | null {
    return this.latest;
  }

  reset(): void {
    this.latest = null;
    this.latestPresentationRevision = -1;
  }

  apply(candidate: PrototypeSnapshot): boolean {
    if (candidate.presentationRevision <= this.latestPresentationRevision) return false;
    this.latest = candidate;
    this.latestPresentationRevision = candidate.presentationRevision;
    return true;
  }

  applyResponse(response: SnapshotResponse): SnapshotUpdate {
    if (response.error === null) {
      if (response.snapshot === null) return { kind: 'none' };
      const applied = this.apply(response.snapshot);
      return { kind: 'success', snapshot: response.snapshot, applied };
    }

    if (response.snapshot === null) {
      // 来源拒绝只返回安全错误，不附聊天数据，也不会推进 revision。
      return { kind: 'failure', error: response.error, snapshot: null, stale: false };
    }

    const stale =
      this.latest !== null && response.snapshot.presentationRevision < this.latestPresentationRevision;
    if (stale) {
      return { kind: 'failure', error: response.error, snapshot: null, stale: true };
    }
    const applied = this.apply(response.snapshot);
    return {
      kind: 'failure',
      error: response.error,
      snapshot: applied ? response.snapshot : this.latest,
      stale: false,
    };
  }
}
