import type {
  BuddyCatalogView,
  BuddyView,
  ChatMessage,
  DiagnosticView,
  MemoryView,
  ModelSettingsView,
  PresetView,
  PrototypeSnapshot,
  SessionCatalogView,
  SessionView,
  SnapshotFailure,
} from './types';
import { buddyCatalogOf } from './buddy';
import { sessionCatalogOf } from './session';

/** 宿主只产生 ready/sending/failed/unknown 四种业务状态；其他字符串原样展示。 */
const STATUS_LABELS: Readonly<Record<string, string>> = {
  ready: '就绪',
  sending: '发送中',
  failed: '发送失败',
  unknown: '结果未知',
};

/**
 * 宿主 failure 是安全失败码。对已知副作用结果给出明确指引，
 * 不用“请重试”之类笼统文案诱导重复发送。
 */
const FAILURE_LABELS: Readonly<Record<string, string>> = {
  send_failed: '上一条消息发送失败，未收到伙伴回复。',
  send_unknown: '上一条消息的结果未知，会话已隔离；请勿重复发送，请重启桌面宿主。',
  send_cancelled: '上一条消息已取消。',
};

/** 撤权后界面必须清场，即使宿主快照里仍保留旧消息或草稿。 */
export function displayMessages(snapshot: PrototypeSnapshot | null): ChatMessage[] {
  if (snapshot === null || snapshot.revoked) return [];
  return snapshot.chat.messages;
}

export function displayDraft(snapshot: PrototypeSnapshot | null): string {
  if (snapshot === null || snapshot.revoked) return '';
  return snapshot.chat.draft;
}

/**
 * 模型配置与记忆数组都属于授权后才投影的业务内容。
 * 撤权时即使宿主还带着旧字段，界面也不得继续展示。
 */
export function displayModelSettings(snapshot: PrototypeSnapshot | null): ModelSettingsView | null {
  if (snapshot === null || snapshot.revoked) return null;
  return snapshot.modelSettings;
}

export function displayPresets(snapshot: PrototypeSnapshot | null): PresetView[] {
  if (snapshot === null || snapshot.revoked) return [];
  return snapshot.presets;
}

export function displayMemories(snapshot: PrototypeSnapshot | null): MemoryView[] {
  if (snapshot === null || snapshot.revoked) return [];
  return snapshot.memories;
}

export function displayDiagnostics(snapshot: PrototypeSnapshot | null): DiagnosticView[] {
  if (snapshot === null || snapshot.revoked) return [];
  return snapshot.diagnostics;
}

export function displayActivePreset(snapshot: PrototypeSnapshot | null): string {
  if (snapshot === null || snapshot.revoked) return '';
  return snapshot.activePreset;
}

export function displayExternalTransmissionAllowed(snapshot: PrototypeSnapshot | null): boolean {
  return snapshot !== null && !snapshot.revoked && snapshot.externalTransmissionAllowed;
}

export function displayPersistenceEnabled(snapshot: PrototypeSnapshot | null): boolean {
  return snapshot !== null && !snapshot.revoked && snapshot.persistenceEnabled;
}

/** 伙伴注册表只在授权有效时展示；撤权或空快照不渲染旧档案。 */
export function displayBuddyCatalog(snapshot: PrototypeSnapshot | null): BuddyCatalogView | null {
  if (snapshot === null || snapshot.revoked) return null;
  return buddyCatalogOf(snapshot);
}

/** 注册表条目只含宿主签发的展示字段与版本；撤权后一律清空。 */
export function displayBuddies(snapshot: PrototypeSnapshot | null): BuddyView[] {
  return displayBuddyCatalog(snapshot)?.buddies ?? [];
}

/** 活动伙伴只由宿主注册表裁决；前端不按昵称、素材或最后编辑推断身份。 */
export function displayActiveBuddy(snapshot: PrototypeSnapshot | null): BuddyView | null {
  const catalog = displayBuddyCatalog(snapshot);
  if (catalog === null || catalog.activeBuddyId === null) return null;
  return catalog.buddies.find((buddy) => buddy.id === catalog.activeBuddyId) ?? null;
}

/**
 * 当前受信档案的人设。未配置模型的伙伴也用它显示/初始化配置表单；
 * 撤权或无活动伙伴时不返回旧人设。
 */
export function displayActiveBuddyPersona(snapshot: PrototypeSnapshot | null): string | null {
  if (snapshot === null || snapshot.revoked) return null;
  if (displayActiveBuddy(snapshot) === null) return null;
  return snapshot.activeBuddyPersona;
}

/** 伙伴/档案/人设/模型版本只做展示，不代表前端获得任何权限。 */
export function buddyRevisionText(buddy: BuddyView | null): string {
  if (buddy === null) return '未签发伙伴';
  return `档案 r${buddy.profileRevision} · 人设 r${buddy.personaRevision} · 模型 r${buddy.modelRevision}`;
}

export function buddyLabel(buddy: BuddyView): string {
  const label = buddy.label.trim();
  return label.length > 0 ? label : buddy.id;
}

/**
 * 会话目录只在授权有效时展示；严格解析已保证字段齐全，
 * 调用方仍需处理空快照与撤权，不渲染空目录或伪造会话。
 */
export function displaySessionCatalog(snapshot: PrototypeSnapshot | null): SessionCatalogView | null {
  if (snapshot === null || snapshot.revoked) return null;
  return sessionCatalogOf(snapshot);
}

/** 隔离新建许可只来自应用裁决；撤权或空快照一律不开放。 */
export function displaySessionIsolationAllowed(snapshot: PrototypeSnapshot | null): boolean {
  return snapshot !== null && !snapshot.revoked && snapshot.sessionIsolationAllowed === true;
}

/** 目录条目只含宿主签发的展示字段；撤权后一律清空。 */
export function displaySessions(snapshot: PrototypeSnapshot | null): SessionView[] {
  return displaySessionCatalog(snapshot)?.sessions ?? [];
}

/** 宿主提示只做去空白展示；空串等价于没有提示。 */
export function displayNotice(snapshot: PrototypeSnapshot | null): string | null {
  if (snapshot === null) return null;
  const text = snapshot.notice;
  if (text === null) return null;
  const trimmed = text.trim();
  return trimmed.length === 0 ? null : trimmed;
}

export function credentialStatusText(settings: ModelSettingsView | null): string {
  if (settings === null) return '未配置模型';
  return settings.hasApiKey ? '本次进程已有 API Key（不会回显或落盘）' : '本次进程没有 API Key';
}

export function transmissionStatusText(allowed: boolean): string {
  return allowed ? '允许向模型端点外传' : '未允许外传';
}

/** 本地保存不可用时给出明确的恢复边界，不假冒已持久化。 */
export function persistenceHint(snapshot: PrototypeSnapshot | null): string | null {
  if (snapshot === null || snapshot.revoked) return null;
  if (snapshot.persistenceEnabled) return null;
  return '本地保存不可用：本次模型配置与记忆不会写入磁盘；请修复数据根后重启桌面宿主。';
}

export function failureText(failure: SnapshotFailure | undefined): string | null {
  if (failure === null || failure === undefined) return null;
  const text = failure.trim();
  if (text.length === 0) return null;
  return FAILURE_LABELS[text] ?? text;
}

export function statusText(status: string | undefined): string {
  if (status === undefined || status.length === 0) return '状态未知';
  return STATUS_LABELS[status.toLowerCase()] ?? status;
}
