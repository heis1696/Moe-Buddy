<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, reactive, ref, watch } from 'vue';
import {
  BUDDY_LABEL_MAX,
  BUDDY_PERSONA_MAX_CHARS,
  BUDDY_REGISTRY_MAX,
  BuddyDraftDiscardWatermarks,
  BuddySessionDrafts,
  buddyCatalogOf,
  buddyCommitMatchesIntent,
  buddyExpectationMatches,
  buddyExpectationOf,
  buddyLabelProblem,
  buddyPersonaProblem,
  isSafeBuddyId,
} from '../core/buddy';
import { chatStatusAllowsSend } from '../core/chatStatus';
import { resolveComposerKey } from '../core/composer';
import MaterialPanel from './MaterialPanel.vue';
import { DraftSyncQueue } from '../core/draftSync';
import {
  closeDialogBody,
  closeDialogConfirmLabel,
  closeDialogTitle,
  managementExitBody,
  managementExitConfirmLabel,
  type ExitIntent,
} from '../core/exitCopy';
import {
  consumeManagementDiscardReceipt,
  ManagementDraftSync,
} from '../core/managementDraftSync';
import {
  describeHostFailure,
  describeThrownFailure,
  formatSafeError,
  isSideEffectOperation,
  operationLabel,
  PrototypeBoundaryError,
  type Operation,
} from '../core/failure';
import { releaseListeners, type Unlisten } from '../core/listeners';
import type { MaterialFrameDescriptor } from '../core/material';
import {
  armMemoryDelete,
  beginMemoryEdit,
  confirmMemoryDelete,
  isMemoryDeleteArmed,
  memoryDeleteFailureText,
  memoryEditDirty,
  memoryEditSuperseded,
  retainMemoryDraft,
  type MemoryDeleteArm,
  type MemoryEditState,
} from '../core/memoryEditor';
import {
  buildConfigureModelCommand,
  endpointOrModelChanged,
  modelFormAfterApply,
  modelFormDirty,
  modelFormFromSettings,
  requiresTransmissionReset,
  type ModelFormState,
} from '../core/modelConfig';
import { PanelSwitchGuard, type PanelTarget } from '../core/panelGuard';
import {
  archiveSessionCommand,
  closeChatCommand,
  cancelManagementExitCommand,
  confirmCloseCommand,
  confirmManagementDiscardCommand,
  createBuddyCommand,
  deleteMemoryCommand,
  hidePanelCommand,
  newSessionCommand,
  openPanelCommand,
  refreshDiagnosticsCommand,
  refreshMemoriesCommand,
  rememberCommand,
  renameBuddyCommand,
  revokeCommand,
  searchMemoriesCommand,
  selectPresetCommand,
  sendCommand,
  setDraftCommand,
  switchBuddyCommand,
  switchSessionCommand,
  updateMemoryCommand,
  type DispatchCommand,
} from '../core/protocol';
import {
  buddyLabel as buddyDisplayLabel,
  buddyRevisionText,
  credentialStatusText,
  displayActiveBuddy,
  displayActiveBuddyPersona,
  displayActivePreset,
  displayBuddyCatalog,
  displayBuddies,
  displayDiagnostics,
  displayDraft,
  displayExternalTransmissionAllowed,
  displayMemories,
  displayMessages,
  displayModelSettings,
  displayNotice,
  displayPersistenceEnabled,
  displayPresets,
  displaySessionCatalog,
  displaySessionIsolationAllowed,
  displaySessions,
  failureText,
  persistenceHint,
  statusText,
  transmissionStatusText,
} from '../core/projection';
import { CommandUnconfirmedLock } from '../core/refresh';
import {
  activeSessionOf,
  isArchivedActiveSession,
  sessionCatalogOf,
  sessionExpectation,
  sessionStatusBlocksSend,
} from '../core/session';
import { clearedSensitiveDrafts } from '../core/sensitiveState';
import { RevisionOrderedSnapshots, type SnapshotUpdate } from '../core/snapshot';
import type {
  BuddyExpectation,
  BuddyView,
  ManagementDraftKind,
  MemoryView,
  ModelSettingsView,
  PanelName,
  PresetView,
  PrototypeError,
  PrototypeSnapshot,
  SessionExpectation,
  SessionView,
  SnapshotResponse,
} from '../core/types';
import {
  dispatchPrototype,
  invokeWindowCommand,
  listenFrameEvents,
  listenSnapshotEvents,
  requestFrame,
  requestSnapshot,
  updateManagementDraftState,
} from '../platform/tauri';

const PANEL_LABELS: Readonly<Record<PanelName, string>> = {
  presets: '模型设置',
  memory: '记忆',
  diagnostics: '运行诊断',
};

function panelLabel(panel: PanelName): string {
  return PANEL_LABELS[panel];
}

/**
 * 六个固定页面是纯前端呈现状态，不新增业务 panel 或 IPC 类型。
 * 记忆/模型/诊断三页各自复用一个既有业务面板；页面切换必须等 Hide/Open
 * 成功回执后才提交，取消、失败或未知都保持原页面与原草稿。
 */
type PageId = 'chat' | 'buddies' | 'appearance' | 'memory' | 'model' | 'diagnostics';

interface PageMeta {
  /** 页面显示名（面包屑、离开确认文案）。 */
  name: string;
  /** 页面主标题（h1，兼作焦点目标）。 */
  title: string;
  /** 侧栏导航文案；与样稿一致用产品名，不牺牲表达迁就旧脚本文案。 */
  nav: string;
}

const PAGES: Readonly<Record<PageId, PageMeta>> = {
  chat: { name: '聊天', title: '聊聊今天', nav: '聊天' },
  buddies: { name: '伙伴', title: '陪伴，各有模样', nav: '伙伴' },
  appearance: { name: '外观与动作', title: '换一种可爱', nav: '外观与动作' },
  memory: { name: '记忆', title: '记住那些小事', nav: '记忆' },
  model: { name: '模型设置', title: '让对话开始', nav: '模型设置' },
  diagnostics: { name: '运行诊断', title: '看看运行情况', nav: '运行诊断' },
};

/** 页面与既有业务面板的唯一对应；纯呈现页为 null。 */
const PAGE_PANELS: Readonly<Record<PageId, PanelTarget | null>> = {
  chat: null,
  buddies: null,
  appearance: null,
  memory: 'memory',
  model: 'presets',
  diagnostics: 'diagnostics',
};

const NAV_GROUPS: ReadonlyArray<{ label: string; pages: readonly PageId[] }> = [
  { label: '日常陪伴', pages: ['chat', 'buddies', 'appearance'] },
  { label: '管理与设置', pages: ['memory', 'model', 'diagnostics'] },
];

/** 各页面导航图标的本地 SVG symbol 后缀。 */
const PAGE_ICONS: Readonly<Record<PageId, string>> = {
  chat: 'chat',
  buddies: 'buddy',
  appearance: 'sparkle',
  memory: 'memory',
  model: 'sliders',
  diagnostics: 'pulse',
};

const snapshots = new RevisionOrderedSnapshots();
const snapshot = ref<PrototypeSnapshot | null>(null);
const connectionState = ref<'loading' | 'ready' | 'failed'>('loading');
const fatalMessage = ref('');
const draft = ref('');
/**
 * 各伙伴各自的按会话草稿缓存：同一局部会话 ID 在不同伙伴下互不共享；
 * 每个伙伴内部仍沿用 A 的按会话有界淘汰。未同步正文在切换伙伴时保留。
 */
const buddyDrafts = new BuddySessionDrafts();
/** 按伙伴隔离的聊天草稿丢弃水位，避免其他伙伴的编辑让旧回执失效。 */
const buddyDiscardWatermarks = new BuddyDraftDiscardWatermarks();
/** 当前伙伴作用域（发起命令时捕获的预期选择与注册表修订）。 */
let buddyTarget: BuddyExpectation = buddyExpectationOf(null);
/** 伙伴作用域代际：切换伙伴、撤权或释放都会推进，旧异步回填不得写回。 */
let buddyScopeGeneration = 0;
/** 当前草稿队列绑定的完整伙伴 context（ID + 选择修订）；迟到回包按它复验。 */
let draftSyncScope: BuddyExpectation = { ...buddyTarget };
/** 当前草稿/发送命令携带的预期会话与目录修订，生命周期成功后按宿主快照更新。 */
let draftTarget: SessionExpectation = { expectedSessionId: null, expectedSessionsRevision: 0 };
const archiveArmed = ref(false);
const busy = ref(false);
/** 副作用命令结果未确认时禁止再次发送或提交，直到重新读取宿主状态。 */
const commandUnconfirmed = ref(false);
/** 记忆、配置、存储、撤权等未知不受会话切换影响，只能由匹配回执收口。 */
const globalUnknown = ref(false);
/**
 * 显式新建隔离会话只由受信应用的 `sessionIsolationAllowed` 开放，
 * 且不能被任何全局未知（保存失败、记忆/配置未知等）绕过。
 * 前端不从 `send` 操作名、聊天状态文案或错误码推导该许可。
 */
const sendIsolationAllowed = computed(
  () => displaySessionIsolationAllowed(snapshot.value) && !globalUnknown.value,
);
const unknownOperationLabel = ref<string | null>(null);
const commandLock = new CommandUnconfirmedLock();
let unconfirmedOperation: Operation | null = null;
const revokeArmed = ref(false);
const lastError = ref<string | null>(null);
const composing = ref(false);
const paused = ref<boolean | null>(null);
const inputRef = ref<HTMLTextAreaElement | null>(null);
const messagesEl = ref<HTMLElement | null>(null);

const activePanel = ref<PanelName | null>(null);
const panelBusy = ref(false);
const panelNotice = ref<string | null>(null);
const pendingPanelLeave = ref<PanelTarget | null>(null);
const panelGuard = new PanelSwitchGuard();
/** 已提交的当前页面；只有面板 Hide/Open 成功回执后才会推进。 */
const activePage = ref<PageId>('chat');
/** 页面切换在途：等待成功回执期间忽略新的导航，连续点击不得串页。 */
const pageNavigating = ref(false);
/** 导航触发的面板离开确认必须记住目标页面；取消时一并丢弃。 */
const pendingLeavePage = ref<PageId | null>(null);
const pendingLeaveLabel = ref<string | null>(null);
let managementDraftSync = createManagementDraftSync();
/** 管理同步状态变化时推进，让依赖它的只读界面重新求值。 */
const managementSyncRevision = ref(0);
/** 最近一次已记录到同步器的本地管理编辑信号，避免回包基线被当成新编辑。 */
let lastManagementDraftSignal: string | null = null;
const modelForm = reactive<ModelFormState>(modelFormFromSettings(null));
const modelApplyNotice = ref<string | null>(null);
const memoryQuery = ref('');
const rememberDraft = ref('');
const memoryEdit = ref<MemoryEditState | null>(null);
const memoryDraft = ref('');
const memoryNotice = ref<string | null>(null);
const memoryError = ref<string | null>(null);
const deleteArm = ref<MemoryDeleteArm | null>(null);
/** 伙伴注册表交互：创建/改名只提交名称与人设，不带 ID、路径或权限字段。 */
const buddyNotice = ref<string | null>(null);
const buddyError = ref<string | null>(null);
const buddyCreateOpen = ref(false);
const buddyRenameOpen = ref(false);
const buddyForm = reactive({ label: '', persona: '' });
const buddyRenameLabel = ref('');

const messages = computed(() => displayMessages(snapshot.value));
const revoked = computed(() => snapshot.value?.revoked === true);
const buddyCatalog = computed(() => displayBuddyCatalog(snapshot.value));
const buddies = computed(() => displayBuddies(snapshot.value));
const activeBuddy = computed(() => displayActiveBuddy(snapshot.value));
const activeBuddyPersona = computed(() => displayActiveBuddyPersona(snapshot.value));
const buddyCatalogAvailable = computed(() => buddyCatalog.value !== null);
const buddiesRevision = computed(() => buddyCatalog.value?.buddiesRevision ?? 0);
const activeBuddyId = computed(() => buddyCatalog.value?.activeBuddyId ?? null);
/** 空串只是“尚无伙伴”哨兵；发送等业务上下文必须要求真实签发的 ID。 */
const hasActiveBuddyContext = computed(
  () => activeBuddyId.value !== null && isSafeBuddyId(activeBuddyId.value),
);
/**
 * 素材命令在发起时捕获当前完整 B context；这里只做只读投影，不消费
 * buddyCommit、不清管理稿，也不解除任何 Unknown。
 */
const materialContext = computed<BuddyExpectation>(() => buddyExpectationOf(snapshot.value));
const sessionCatalog = computed(() => displaySessionCatalog(snapshot.value));
const sessions = computed(() => displaySessions(snapshot.value));
const activeSession = computed(() => activeSessionOf(snapshot.value));
const activeSessionArchived = computed(() => isArchivedActiveSession(snapshot.value));
const sessionCatalogAvailable = computed(() => sessionCatalog.value !== null);
const sessionsRevision = computed(() => sessionCatalog.value?.sessionsRevision ?? 0);
const activeSessionId = computed(() => sessionCatalog.value?.activeSessionId ?? null);
const sessionDirectoryRevision = computed(() => (sessionCatalogAvailable.value ? sessionsRevision.value : null));
const sessionSendingInFlight = computed(
  () =>
    snapshot.value?.chat.status.trim().toLowerCase() === 'sending' ||
    activeSession.value?.status === 'sending',
);
const sessionStatusUnknown = computed(() => sessionStatusBlocksSend(snapshot.value));
const closeConfirmation = computed(() => snapshot.value?.closeConfirmation === true);
const closeIntent = computed<ExitIntent | null>(() => snapshot.value?.closeIntent ?? null);
/** 关闭确认所属伙伴；确认动作必须仍在该伙伴作用域内，避免旧确认作用于新伙伴。 */
const closeDialogBuddyId = ref<string | null>(null);
const managementExitConfirmation = computed(() => snapshot.value?.managementExit ?? null);
const materialBlockReason = computed<string | null>(() => {
  if (connectionState.value !== 'ready' || snapshot.value === null) return '尚未连接桌面宿主。';
  if (revoked.value) return '授权已撤回，没有可用的伙伴上下文。';
  if (!hasActiveBuddyContext.value) return '需要当前伙伴上下文后才能选择素材。';
  if (closeConfirmation.value || managementExitConfirmation.value !== null) {
    return '关闭/退出确认处理中，暂不能变更素材。';
  }
  if (revokeArmed.value) return '撤回授权确认处理中，暂不能变更素材。';
  return null;
});
const chatFailure = computed(() => failureText(snapshot.value?.chat.failure));
const chatStatus = computed(() => snapshot.value?.chat.status.trim().toLowerCase() ?? '');
const sendAllowed = computed(() => chatStatusAllowsSend(chatStatus.value));
const statusLabel = computed(() => (revoked.value ? '已撤回授权' : statusText(chatStatus.value)));
const statusTone = computed(() => {
  if (revoked.value || chatStatus.value === 'failed') return 'tone-danger';
  if (chatStatus.value === 'unknown') return 'tone-warn';
  if (chatStatus.value === 'sending') return 'tone-busy';
  return 'tone-ok';
});
const interactionLocked = computed(
  () =>
    busy.value ||
    closeConfirmation.value ||
    managementExitConfirmation.value !== null ||
    revokeArmed.value,
);
// 注册表 Unknown 仍允许撤权；入口不能依赖管理面板打开成功或普通业务许可。
const canRequestRevoke = computed(
  () => connectionState.value === 'ready' && snapshot.value !== null &&
    !revoked.value && !interactionLocked.value && !panelBusy.value,
);
const canCompose = computed(
  () =>
    connectionState.value === 'ready' &&
    snapshot.value !== null &&
    !revoked.value &&
    !activeSessionArchived.value &&
    !interactionLocked.value,
);
const canSend = computed(
  () =>
    canCompose.value &&
    hasActiveBuddyContext.value &&
    !commandUnconfirmed.value &&
    !sessionStatusUnknown.value &&
    sendAllowed.value &&
    draft.value.trim().length > 0,
);
const canMutate = computed(() => canCompose.value && !commandUnconfirmed.value && !panelBusy.value);
const panelActionsAllowed = computed(
  () =>
    connectionState.value === 'ready' &&
    snapshot.value !== null &&
    !revoked.value &&
    !closeConfirmation.value &&
    !revokeArmed.value &&
    managementExitConfirmation.value === null,
);

/** 生命周期动作的共同前置；在途发送或关闭确认期间明确拒绝，由界面说明原因。 */
const sessionActionBlockReason = computed<string | null>(() => {
  if (connectionState.value !== 'ready' || snapshot.value === null) return '尚未连接桌面宿主。';
  if (revoked.value) return '授权已撤回，没有可用的会话目录。';
  if (!sessionCatalogAvailable.value) {
    return '桌面宿主尚未提供会话目录与目录修订，无法安全新建、切换或归档会话。';
  }
  if (closeConfirmation.value || managementExitConfirmation.value !== null) {
    return '关闭/退出确认处理中，暂不能变更会话。';
  }
  if (revokeArmed.value) return '撤回授权确认处理中，暂不能变更会话。';
  if (busy.value) return '当前有未完成的操作，暂不能变更会话。';
  if (panelBusy.value) return '管理面板操作尚未确认，暂不能变更会话。';
  if (sessionSendingInFlight.value) {
    return '当前会话仍有在途发送，宿主未确认结束，暂不能变更会话。';
  }
  return null;
});

const newSessionBlockReason = computed<string | null>(() => {
  const base = sessionActionBlockReason.value;
  if (base !== null) return base;
  if (globalUnknown.value) {
    return `存在未确认的${unknownOperationLabel.value ?? '全局副作用'}操作；新建会话不能解除记忆、配置、存储、撤权等未知锁。`;
  }
  // 宿主仍报告当前会话未知（含保存失败叠加 send_unknown）却不给隔离许可时，
  // 不能把“聊天状态未知”当成可以另开。正常无未知的新建不受此限。
  const statusUnknown = chatStatus.value === 'unknown' || sessionStatusUnknown.value;
  if (statusUnknown && !sendIsolationAllowed.value) {
    return '当前会话仍为未知状态，或叠加了存储/保存失败等全局未知；应用未开放显式隔离新建。';
  }
  if (commandUnconfirmed.value && !sendIsolationAllowed.value) {
    return '当前会话的外部结果尚未确认本地处理结束，或叠加了存储/保存失败等全局未知；应用未开放显式隔离新建。';
  }
  return null;
});

const switchSessionBlockReason = computed<string | null>(() => {
  const base = sessionActionBlockReason.value;
  if (base !== null) return base;
  if (commandUnconfirmed.value) {
    return '存在未确认的副作用操作；切换会话不能解除 Unknown，请先重新读取状态。';
  }
  return null;
});

/** 创建/改名的本地可见编辑；打开空表单本身不构成 dirty。 */
const buddyCreateDirty = computed(
  () => buddyCreateOpen.value && (buddyForm.label.length > 0 || buddyForm.persona.length > 0),
);
const buddyRenameDirty = computed(
  () =>
    buddyRenameOpen.value &&
    activeBuddy.value !== null &&
    buddyRenameLabel.value !== activeBuddy.value.label,
);
const managementFormDirty = computed(() => buddyCreateDirty.value || buddyRenameDirty.value);
const managementSync = computed(() => {
  // 读取修订让同步器的异步回执也能使依赖方重新求值。
  void managementSyncRevision.value;
  return {
    version: managementDraftSync.version,
    dirty: managementDraftSync.dirty,
    kind: managementDraftSync.kind,
    synced: managementDraftSync.isSynced,
  };
});

/**
 * 本地受保护管理草稿的单一来源：同一时刻只允许一个种类处于 dirty。
 * 表单打开空内容时回到 none，与冻结契约 `dirty=false 只能是 none` 一致。
 */
const managementDraftIntent = computed<{ kind: ManagementDraftKind; dirty: boolean }>(() => {
  if (buddyCreateDirty.value) return { kind: 'buddy_create', dirty: true };
  if (buddyRenameDirty.value) return { kind: 'buddy_rename', dirty: true };
  if (panelDirty.value) return { kind: 'general', dirty: true };
  return { kind: 'none', dirty: false };
});

/** 所有会改变管理编辑信号的本地值；用于区分真实编辑与回包基线刷新。 */
const managementDraftSignal = computed(() =>
  JSON.stringify([
    buddyCreateOpen.value,
    buddyForm.label,
    buddyForm.persona,
    buddyRenameOpen.value,
    buddyRenameLabel.value,
    activePanel.value,
    modelForm.endpoint,
    modelForm.model,
    modelForm.persona,
    modelForm.allowTransmission,
    modelForm.apiKeyInput,
    modelForm.clearApiKey,
    modelForm.transmissionTouched,
    memoryEdit.value?.id ?? '',
    memoryEdit.value?.revision ?? 0,
    memoryDraft.value,
    rememberDraft.value,
  ]),
);

/** 管理面板、伙伴表单、同步器与权威 dirty 位都阻止伙伴作用域变化。 */
const managementDirty = computed(
  () =>
    managementFormDirty.value ||
    panelDirty.value ||
    managementSync.value.dirty ||
    snapshot.value?.managementDraft.dirty === true,
);

/**
 * 不涉及管理草稿的共同前置：连接、授权、注册表、在途执行与全局未知。
 * 创建/改名会单独放行自己的表单种类。
 */
const baseBuddyBlockReason = computed<string | null>(() => {
  if (connectionState.value !== 'ready' || snapshot.value === null) return '尚未连接桌面宿主。';
  if (revoked.value) return '授权已撤回，没有可用的伙伴注册表。';
  if (!buddyCatalogAvailable.value) {
    return '桌面宿主尚未提供伙伴注册表与选择修订，无法安全创建、切换或改名伙伴。';
  }
  if (closeConfirmation.value || managementExitConfirmation.value !== null) {
    return '关闭/退出确认处理中，暂不能变更伙伴。';
  }
  if (revokeArmed.value) return '撤回授权确认处理中，暂不能变更伙伴。';
  if (busy.value) return '当前有未完成的操作，暂不能变更伙伴。';
  if (panelBusy.value) return '管理面板操作尚未确认，暂不能变更伙伴。';
  if (sessionSendingInFlight.value) {
    return '当前会话仍有在途发送，宿主未确认结束，暂不能变更伙伴。';
  }
  if (globalUnknown.value) {
    return `存在未确认的${unknownOperationLabel.value ?? '全局副作用'}操作；变更伙伴不能解除记忆、配置、存储、撤权等未知锁。`;
  }
  return null;
});

/** 创建/改名只放行自己种类且已精确同步的表单，其他编辑仍阻止。 */
function buddyFormBlockReason(kind: 'buddy_create' | 'buddy_rename'): string | null {
  if (panelDirty.value) {
    return '管理面板有未保存编辑；请先保存或显式放弃后再使用伙伴表单。';
  }
  const intent = managementDraftIntent.value;
  if (intent.dirty && intent.kind !== kind) {
    return '另一份伙伴表单有未保存编辑；请先保存或取消后再打开当前表单。';
  }
  if (intent.dirty) {
    const sync = managementSync.value;
    if (!sync.dirty || sync.kind !== kind) {
      return '伙伴表单尚未同步到桌面宿主，暂不能保存。';
    }
    if (!sync.synced) {
      return '伙伴表单的退出保护状态尚未同步到桌面宿主，暂不能保存。';
    }
  } else if (managementSync.value.dirty) {
    return '存在尚未确认的管理草稿状态；请先重新读取宿主状态。';
  }
  return null;
}

/**
 * 切换伙伴：拒绝任何未保存管理稿（面板、创建、改名或未确认同步）。
 * 创建/改名各自的保存不走这里，避免自己的表单反过来阻止提交。
 */
const buddyActionBlockReason = computed<string | null>(() => {
  const base = baseBuddyBlockReason.value;
  if (base !== null) return base;
  if (managementDirty.value) {
    return '管理面板或伙伴表单有未保存编辑或尚未确认的编辑状态；请先保存或显式放弃后再变更伙伴。';
  }
  if (!managementSync.value.synced) {
    return '管理编辑状态尚未同步到桌面宿主，暂不能变更伙伴。';
  }
  return null;
});

/** 新建伙伴额外要求注册表未满；上限由受信服务裁决，界面只提前说明。 */
const createBuddyBlockReason = computed<string | null>(() => {
  const base = baseBuddyBlockReason.value;
  if (base !== null) return base;
  const form = buddyFormBlockReason('buddy_create');
  if (form !== null) return form;
  if (buddies.value.length >= BUDDY_REGISTRY_MAX) {
    return `首批最多 ${BUDDY_REGISTRY_MAX} 个伙伴；本阶段不提供删除入口。`;
  }
  return null;
});

/** 改名要求存在受信活动伙伴，且只放行改名表单自己的精确版本。 */
const renameBuddyBlockReason = computed<string | null>(() => {
  const base = baseBuddyBlockReason.value;
  if (base !== null) return base;
  if (activeBuddy.value === null) return '宿主未签发活动伙伴，无法改名。';
  return buddyFormBlockReason('buddy_rename');
});

const archiveBlockReason = computed<string | null>(() => {
  const base = switchSessionBlockReason.value;
  if (base !== null) return base;
  if (activeSession.value === null) return '宿主未签发当前会话，无法归档。';
  if (activeSession.value.archived) return '当前会话已归档。';
  return null;
});
const sendBlockade = computed(() => {
  if (revoked.value || snapshot.value === null) return null;
  if (!hasActiveBuddyContext.value) {
    return '宿主未签发可用的伙伴身份；空哨兵不能作为发送的业务上下文。';
  }
  if (activeSessionArchived.value) {
    return '当前会话已归档，只能只读查看；不能发送或继续编辑草稿，本阶段不提供解归档。';
  }
  if (sessionStatusUnknown.value) {
    return '会话目录中的当前会话状态为未知/在途，宿主未确认可发送；请先重新读取状态。';
  }
  if (chatStatus.value === 'unknown') {
    if (commandLock.source === 'ipc_reply_lost') {
      return '上一次命令回包未收到，发送保持锁定；普通状态重读不能确认其结果。';
    }
    return '业务结果未知，发送保持锁定；普通状态重读不能解除未知，请勿重复发送。';
  }
  if (chatStatus.value === 'sending') {
    return '消息仍在发送中，等待桌面宿主确认后才能再次发送。';
  }
  if (!sendAllowed.value) {
    return `当前聊天状态“${statusText(chatStatus.value)}”不可发送，请先确认宿主状态。`;
  }
  return null;
});
const pauseLabel = computed(() => {
  if (paused.value === null) return '暂停 / 继续宠物';
  return paused.value ? '继续宠物' : '暂停宠物';
});

const modelSettings = computed<ModelSettingsView | null>(() => displayModelSettings(snapshot.value));
const presets = computed(() => displayPresets(snapshot.value));
const memories = computed(() => displayMemories(snapshot.value));
const diagnostics = computed(() => displayDiagnostics(snapshot.value));
const activePreset = computed(() => displayActivePreset(snapshot.value));
const externalTransmissionAllowed = computed(() =>
  displayExternalTransmissionAllowed(snapshot.value),
);
const persistenceEnabled = computed(() => displayPersistenceEnabled(snapshot.value));
const hostNotice = computed(() => displayNotice(snapshot.value));
const persistenceNotice = computed(() => persistenceHint(snapshot.value));
const modelChangeHint = computed(() => {
  if (!endpointOrModelChanged(modelSettings.value, modelForm)) return null;
  const credentialHandled = modelForm.apiKeyInput.length > 0 || modelForm.clearApiKey;
  const transmissionHandled =
    modelSettings.value?.allowTransmission !== true || modelForm.transmissionTouched;
  if (credentialHandled && transmissionHandled) return null;
  return '端点或模型已变化：旧 API Key 不会自动沿用，发送许可也需要重新确认。';
});
const memoryPanelDirty = computed(
  () => memoryEditDirty(memoryEdit.value, memoryDraft.value) || rememberDraft.value.trim().length > 0,
);
const modelPanelDirty = computed(() =>
  modelFormDirty(modelSettings.value, modelForm, activeBuddyPersona.value),
);
const panelDirty = computed(() => {
  if (activePanel.value === 'presets') return modelPanelDirty.value;
  if (activePanel.value === 'memory') return memoryPanelDirty.value;
  return false;
});
const pendingLeaveDescription = computed(() => {
  if (pendingPanelLeave.value === null) return '';
  if (pendingPanelLeave.value === 'close') {
    const label = pendingLeaveLabel.value;
    return label === null
      ? '关闭当前面板会丢弃未保存的修改。'
      : `切换到「${label}」会丢弃未保存的修改。`;
  }
  return `切换到「${pendingLeaveLabel.value ?? panelLabel(pendingPanelLeave.value)}」会丢弃未保存的修改。`;
});
const memoryEditStale = computed(() => memoryEditSuperseded(memoryEdit.value, memories.value));

let disposed = false;
let initializing = false;
let receivedSuccessfulSnapshot = false;
let modelInteractionGeneration = 0;
let managementInteractionGeneration = 0;
let unlisteners: Unlisten[] = [];
let frameEventCount = 0;
/** null 表示与 revision 无关的固定错误；数字表示错误携带的最新快照 revision。 */
let lastErrorRevision: number | null = null;
let draftSyncEpoch = 0;
/**
 * 本地草稿编辑/交互代际：每次用户输入、会话或伙伴作用域切换、撤权清理都推进。
 * 失败回包只在派发后没有任何新编辑时才能恢复原稿。
 */
let draftEditGeneration = 0;
let draftSync = createDraftSync();
/**
 * 本次显式模型保存发起时捕获的完整证据。只有匹配的 buddyCommit 才能消费它：
 * 事件先到、回复先到或回复丢失后重读都走同一份证据；新编辑或无关迟到结果
 * 只结束本次意图，不清当前表单。
 */
interface ModelCommitIntent {
  kind: 'configure_model';
  fromBuddyId: string;
  fromRevision: number;
  managementVersion: number;
  interaction: number;
  applied: boolean;
}
let modelCommitIntent: ModelCommitIntent | null = null;

/**
 * 伙伴创建/改名/切换发起时捕获的完整证据。事件与调用回复走同一份证据：
 * - 只有动作、发起伙伴、发起修订、管理版本都匹配的 `buddyCommit` 才能消费；
 * - 表单是否清除由**本地编辑代际**决定：表单正文/信号或管理版本在提交后推进时
 *   只确认已提交版本，提交后新增的名称/人设保留为新的未提交稿；
 * - 事件先到与回复先到共用同一消费路径，重复事件/重复回复不能再次清稿；
 * - 更新稿在作用域切换后仍归属新作用域并重新登记退出保护，不自动应用到另一伙伴。
 */
interface BuddyCommitIntent {
  kind: 'create_buddy' | 'switch_buddy' | 'rename_buddy';
  fromBuddyId: string;
  fromRevision: number;
  managementVersion: number;
  submittedForm: BuddyFormCapture;
  submittedSignal: string;
  submittedSync: ManagementDraftSync;
  submittedSyncVersion: number;
  applied: boolean;
  newerEdits: boolean;
  preservedForm: boolean;
  formSettled: boolean;
  protectionRecorded: boolean;
}
let buddyCommitIntent: BuddyCommitIntent | null = null;

function setError(text: string, revision: number | null): void {
  lastError.value = text;
  lastErrorRevision = revision;
}

/**
 * 管理编辑同步实例绑定**完整伙伴作用域**（ID + 选择修订）。派发只使用创建时
 * 捕获的作用域，绝不在这里重新读取“当前上下文”：否则排队中的旧编辑会在选择
 * 修订推进后拿到新修订。修订推进时由 `adoptBuddyScope` 取消旧实例并用新作用域
 * 重建；旧实例既不再派发也不再采纳迟到回执。
 * 种类是必填同步载荷，回执仍只有 `{ version, dirty }`。
 */
function createManagementDraftSync(): ManagementDraftSync {
  const scope = buddyTarget;
  return new ManagementDraftSync(
    scope,
    async (version, dirty, kind) => {
      const current = buddyExpectationOf(snapshots.current);
      if (!buddyExpectationMatches(current, scope)) {
        throw new PrototypeBoundaryError(
          'buddy_scope_changed',
          '管理编辑状态所属伙伴作用域已变化，本次同步未执行',
        );
      }
      return updateManagementDraftState(version, dirty, kind, scope);
    },
    () => {
      if (!disposed && snapshots.current?.revoked !== true) {
        setError('管理编辑状态未能确认同步，退出保护仍待宿主确认。', null);
      }
    },
    () => {
      managementSyncRevision.value += 1;
    },
  );
}

function syncCommandUnconfirmed(): void {
  commandUnconfirmed.value = commandLock.locked;
  globalUnknown.value = commandLock.hasGlobalUnknown;
  unknownOperationLabel.value =
    commandLock.operation === null ? null : operationLabel(commandLock.operation as Operation);
}

function markCommandUnknown(
  operation: Operation,
  source: 'ipc_reply_lost' | 'business_unknown',
  sessionId: string | null = null,
  buddyId: string = buddyTarget.expectedBuddyId,
): void {
  unconfirmedOperation = operation;
  commandLock.markUnknown(operation, source, sessionId, buddyId);
  syncCommandUnconfirmed();
}

function clearError(): void {
  lastError.value = null;
  lastErrorRevision = null;
}

function responseCatalogMismatch(
  response: SnapshotResponse,
  expected: SessionExpectation,
): boolean {
  // 撤权是单调的：即使会话预期不匹配，也必须让撤权快照进入统一清理路径。
  if (response.snapshot?.revoked === true) return false;
  const catalog = sessionCatalogOf(response.snapshot);
  if (catalog === null) return false;
  return catalog.activeSessionId !== expected.expectedSessionId;
}

/** 迟到的伙伴回包按发起时捕获的伙伴比较，不拿“当前伙伴”替换。 */
function responseBuddyMismatch(response: SnapshotResponse, buddyId: string): boolean {
  if (response.snapshot?.revoked === true) return false;
  const catalog = buddyCatalogOf(response.snapshot);
  if (catalog === null) return false;
  return (catalog.activeBuddyId ?? '') !== buddyId;
}

/**
 * 生命周期失败分支的恢复凭据：派发时的会话 ID、目录修订与本地编辑代际。
 * 只比较 ID 不够：同 ID 新修订、A→B→A、派发后继续编辑都必须拒绝回填旧稿。
 */
interface SessionDraftCapture {
  expectedSessionId: string | null;
  expectedSessionsRevision: number;
  draftEditGeneration: number;
}

/**
 * 只有当前权威快照仍指向派发时的同一会话与同一目录修订、未撤权，且本地没有
 * 新编辑时，失败分支才可恢复原会话草稿；否则保留宿主/用户更新的状态。
 */
function currentSessionStill(capture: SessionDraftCapture): boolean {
  if (disposed || snapshots.current?.revoked === true) return false;
  if (draftEditGeneration !== capture.draftEditGeneration) return false;
  const catalog = sessionCatalogOf(snapshots.current);
  if (catalog === null) return capture.expectedSessionId === null;
  return catalog.activeSessionId === capture.expectedSessionId
    && catalog.sessionsRevision === capture.expectedSessionsRevision;
}

function handleDraftSettled(
  text: string,
  response: SnapshotResponse | null,
  error: unknown,
): void {
  if (disposed) return;
  // 队列创建时绑定的完整伙伴 context，不能用“当前伙伴/当前修订”替换。
  const capturedScope = draftSyncScope;
  if (response !== null) {
    if (responseBuddyMismatch(response, capturedScope.expectedBuddyId)) {
      setError('收到其他伙伴的草稿回包，已忽略对当前伙伴的回填；当前草稿保留。', null);
      return;
    }
    if (responseCatalogMismatch(response, draftTarget)) {
      setError('收到其他会话的草稿回包，已忽略对当前会话的回填；当前草稿保留。', null);
      return;
    }
    if (sessionResponseIsStale(response, draftTarget)) {
      setError('收到较旧会话目录修订的草稿回包，已忽略回填；当前草稿保留。', null);
      return;
    }
    applyResponse(response, 'draft');
    // 宿主已接受这份正文：记录该伙伴该会话的权威草稿修订，之后的丢弃水位
    // 不能清掉晚于回执的已确认编辑。旧回包中的正文从未被写回本地。
    const targetId = draftTarget.expectedSessionId;
    if (targetId !== null) {
      buddyDrafts.markSynced(
        capturedScope.expectedBuddyId,
        targetId,
        text,
        draftSessionRevision(snapshots.current, targetId),
      );
    }
    return;
  }
  setError(describeThrownFailure('draft', error), null);
}

function createDraftSync(): DraftSyncQueue {
  const epoch = ++draftSyncEpoch;
  const scope: BuddyExpectation = { ...buddyTarget };
  draftSyncScope = scope;
  return new DraftSyncQueue({
    // 每个请求在派发时读取当前会话预期，但伙伴 context 必须是创建队列时捕获的
    // 完整值；同一伙伴的修订推进也不能让旧排队草稿改用新修订发出。
    dispatch: (text) => {
      const current = buddyExpectationOf(snapshots.current);
      if (!buddyExpectationMatches(current, scope)) {
        throw new PrototypeBoundaryError(
          'buddy_scope_changed',
          '草稿所属伙伴作用域已变化，本次草稿同步未执行',
        );
      }
      return dispatchPrototype(setDraftCommand(text, { ...draftTarget }), current);
    },
    onSettled: (text, response, error) => {
      if (epoch !== draftSyncEpoch) return;
      handleDraftSettled(text, response, error);
    },
  });
}

/**
 * 读取某会话的权威草稿修订，不用其他会话推进的全局修订代替。
 */
function draftSessionRevision(
  current: PrototypeSnapshot | null,
  sessionId: string | null,
): number {
  if (current === null || sessionId === null) return 0;
  const sessions = current.chatDraft.sessions;
  const entry = sessions.find((session) => session.id === sessionId);
  return entry?.revision ?? 0;
}

function rememberSessionDraft(
  sessionId: string,
  text: string,
  synced: boolean = draftSync.isSynced(),
  buddyId: string = buddyTarget.expectedBuddyId,
  current: PrototypeSnapshot | null = snapshot.value,
): void {
  buddyDrafts.remember(
    buddyId,
    sessionId,
    text,
    draftSessionRevision(current, sessionId),
    draftEditGeneration,
    synced,
  );
}

function rememberCurrentDraft(text: string): void {
  const id = activeSessionId.value;
  if (id === null) return;
  // 每次输入都是本地未同步代际；宿主确认后由 handleDraftSettled 提升。
  rememberSessionDraft(id, text, false);
}

/** 只保留当前注册表仍列出的伙伴作用域；撤权或坏快照直接清空。 */
function pruneBuddyScopes(current: PrototypeSnapshot | null): void {
  const catalog = buddyCatalogOf(current);
  if (catalog === null) {
    buddyDrafts.clear();
    buddyDiscardWatermarks.clear();
    return;
  }
  const keep = catalog.buddies.map((buddy) => buddy.id);
  keep.push(buddyTarget.expectedBuddyId);
  buddyDrafts.prune(keep);
  buddyDiscardWatermarks.prune(keep);
}

/**
 * 宿主注册表的活动伙伴变化时：
 * - 先把旧伙伴输入框正文留在旧伙伴自己的槽位，绝不搬运到新伙伴；
 * - 推进伙伴/草稿交互代际，取消并重建绑定新伙伴的草稿与管理同步队列；
 * - 清空只属于旧伙伴的面板、API Key 输入、管理编辑与确认缓存；
 * - 随后只从新伙伴自己的宿主快照恢复，不自动回填旧伙伴的任何正文或凭据。
 *
 * 同一伙伴的选择修订推进也视为作用域变化（A→B→A 不能只看 ID）：
 * - 旧的管理同步队列可能仍持有按旧修订记录的编辑，必须取消，不能给旧编辑
 *   换上新修订；取消只丢弃队列状态，不触碰当前表单正文；
 * - 用新修订重建管理同步并采纳宿主权威状态（有证据的受控协调）；在用户于新
 *   作用域重新编辑之前不再自动补发旧编辑；
 * - 推进作用域代际，让旧修订下在途的模型/记忆/诊断回填失去资格；
 * - 聊天草稿按会话归属，不随伙伴选择修订失效；其回填与恢复仍按会话目录修订复验。
 */
function adoptBuddyScope(previousSnapshot: PrototypeSnapshot | null): void {
  const current = snapshots.current;
  const next = buddyExpectationOf(current);
  const previousId = buddyTarget.expectedBuddyId;
  if (buddyExpectationMatches(next, buddyTarget)) {
    return;
  }
  // 本次作用域变化若来自正在确认的创建/改名提交，不能先清掉提交后新增的
  // 名称/人设；是否清稿统一由 buddyCommit/本地编辑代际消费决定。
  const preserveCommittedForm = pendingBuddyCommitChangesScope(previousSnapshot, current);
  if (next.expectedBuddyId === previousId) {
    // 同一伙伴、仅选择修订推进：取消旧队列，保稿重建并采纳宿主当前状态。
    // 聊天草稿队列也绑定完整 context：旧修订下在途/待发的正文不得自动改用
    // 新修订重发，但输入框里尚未确认的本地正文必须保留，等用户下一次编辑。
    const hasUnsyncedDraft = !draftSync.isSynced();
    const retainedDraft = draft.value;
    buddyTarget = next;
    buddyScopeGeneration += 1;
    draftEditGeneration += 1;
    draftSync.cancel();
    draftSync = createDraftSync();
    if (hasUnsyncedDraft) draftSync.retainUnsynced(retainedDraft);
    managementDraftSync.cancel();
    managementDraftSync = createManagementDraftSync();
    managementSyncRevision.value += 1;
    if (current !== null) {
      managementDraftSync.adopt(current.managementDraft, next);
    }
    // 修订推进不是新编辑：把当前信号记为基线，等用户真正改动时再按新作用域记录。
    lastManagementDraftSignal = managementDraftSignal.value;
    if (preserveCommittedForm) recordPreservedBuddyCommitForm();
    return;
  }
  if (!disposed && draftTarget.expectedSessionId !== null) {
    rememberSessionDraft(
      draftTarget.expectedSessionId,
      draft.value,
      draftSync.isSynced(),
      previousId,
      previousSnapshot,
    );
  }
  buddyScopeGeneration += 1;
  draftEditGeneration += 1;
  draftSync.cancel();
  buddyTarget = next;
  // 撤权/释放后原伙伴作用域关闭：未知锁随作用域迁移，绝不把撤权当成已收口。
  if (next.expectedBuddyId === '') {
    commandLock.migrateGlobalUnknown(previousId, next.expectedBuddyId);
  }
  // 旧实例的排队同步不得落到新伙伴；旧实例自身的回包也不再被采纳。
  managementDraftSync.cancel();
  managementDraftSync = createManagementDraftSync();
  managementSyncRevision.value += 1;
  // 结束旧伙伴的会话作用域；恢复交给 adoptSessionScope 只读新伙伴的槽位。
  draftTarget = { expectedSessionId: null, expectedSessionsRevision: 0 };
  draftSync = createDraftSync();
  draft.value = '';
  // 面板与管理编辑属于旧伙伴：直接结束，不把未保存内容或 API Key 带入新伙伴。
  activePanel.value = null;
  if (PAGE_PANELS[activePage.value] !== null) activePage.value = 'chat';
  pendingPanelLeave.value = null;
  pendingLeavePage.value = null;
  pendingLeaveLabel.value = null;
  panelGuard.cancel();
  panelNotice.value = null;
  resetPanelMemoryState();
  discardPanelDrafts();
  syncModelForm(true);
  if (!preserveCommittedForm) {
    buddyCreateOpen.value = false;
    buddyRenameOpen.value = false;
    buddyForm.label = '';
    buddyForm.persona = '';
    buddyRenameLabel.value = '';
  }
  buddyNotice.value = null;
  buddyError.value = null;
  archiveArmed.value = false;
  pruneBuddyScopes(current);
  if (current !== null) {
    managementDraftSync.adopt(current.managementDraft, next);
  }
  // 作用域切换后以新的已记录信号为基线：清场不是新编辑，不能再推进保护版本。
  lastManagementDraftSignal = managementDraftSignal.value;
  if (preserveCommittedForm) recordPreservedBuddyCommitForm();
}

/** 关闭确认出现时绑定当前伙伴；确认框消失后清除绑定。 */
function syncCloseDialogScope(): void {
  if (snapshot.value?.closeConfirmation !== true) {
    closeDialogBuddyId.value = null;
    return;
  }
  if (closeDialogBuddyId.value === null) {
    closeDialogBuddyId.value = buddyTarget.expectedBuddyId;
  }
}

/**
 * 宿主快照的当前会话变化时：
 * - 先把旧会话草稿留在本伙伴本会话自己的槽位，绝不搬运到新会话；
 * - 停掉旧草稿队列（含尚未派发的输入），按新会话预期重建；
 * - 只恢复本伙伴新会话自己的草稿；没有则用宿主草稿，不伪造正文。
 */
function adoptSessionScope(): void {
  const current = snapshots.current;
  const catalog = sessionCatalogOf(current);
  const next: SessionExpectation =
    catalog === null
      ? { expectedSessionId: null, expectedSessionsRevision: 0 }
      : {
          expectedSessionId: catalog.activeSessionId,
          expectedSessionsRevision: catalog.sessionsRevision,
        };
  const previousId = draftTarget.expectedSessionId;
  const changed = next.expectedSessionId !== previousId;
  draftTarget = next;
  if (!changed) return;
  // 会话作用域变化也是一次交互代际：此前派发的失败回包不得再回填旧稿。
  if (previousId !== null && !disposed) {
    // 记录旧会话自己的同步修订、本地编辑代际与是否已确认；未同步正文
    // 在之后收到该会话的丢弃水位时仍保留，不因切换被误清。
    rememberSessionDraft(previousId, draft.value, draftSync.isSynced());
  }
  draftEditGeneration += 1;
  draftSync.cancel();
  draftSync = createDraftSync();
  const stored =
    next.expectedSessionId === null
      ? null
      : buddyDrafts.recallEntry(buddyTarget.expectedBuddyId, next.expectedSessionId);
  // 空槽位不是“清稿回执”：宿主非空草稿仍优先，避免用本地空串覆盖权威正文。
  const restored = stored !== null && stored.text.length > 0 ? stored.text : null;
  const hostDraft = displayDraft(current);
  draft.value = restored !== null ? restored : hostDraft;
  if (
    !disposed &&
    restored !== null &&
    restored !== hostDraft &&
    !isArchivedActiveSession(current)
  ) {
    // 恢复的是该会话自己的草稿；只同步草稿，不重发已结束或未知的请求。
    draftSync.enqueue(restored);
  }
}

/**
 * 消费应用发出的权威聊天草稿丢弃回执：
 * - 以 `chatDraft.sessions` 的每会话水位为准逐会话消费，其他会话编辑推进
 *   全局 revision 不会让本会话的旧回执失效；
 * - 水位按伙伴隔离，其他伙伴的编辑或丢弃不会让本伙伴的旧回执失效；
 * - 每会话水位独立幂等，漏收中间事件后仍按最新水位补清；
 * - 只清同步修订早于水位的已同步缓存；晚于水位的已确认编辑和未同步正文、
 *   其他会话草稿都保留；
 * - 清当前会话时同时失效旧草稿队列与编辑代际，旧回包不能再把已丢弃正文写回；
 * - 消费必须先于会话恢复入口，绝不能先自动 set_draft 再清。
 */
function consumeChatDraftDiscard(current: PrototypeSnapshot | null): void {
  if (current === null || current.revoked) return;
  const buddyId = buddyTarget.expectedBuddyId;
  // 输入框当前代表的是 draftTarget 绑定的会话；同一快照可能已切换到别的
  // 会话，因此不能用目录里的新 active 判断输入框是否被丢弃。
  const composerId = draftTarget.expectedSessionId;
  const watermarks = new Map<string, number>();
  for (const session of current.chatDraft.sessions) {
    if (session.discardedRevision === null) continue;
    const known = watermarks.get(session.id) ?? 0;
    if (session.discardedRevision > known) watermarks.set(session.id, session.discardedRevision);
  }
  if (watermarks.size === 0) return;
  const composerUnsynced = composerId !== null && !draftSync.isSynced();
  let composerDiscarded = false;
  for (const [sessionId, watermark] of watermarks) {
    const consumed = buddyDiscardWatermarks.get(buddyId, sessionId);
    if (watermark <= consumed) continue;
    buddyDiscardWatermarks.raise(buddyId, sessionId, watermark);
    const entry = buddyDrafts.peek(buddyId, sessionId);
    if (entry === null) {
      // 输入框没有缓存（例如超长正文未另存副本）：未同步正文仍保留。
      if (sessionId === composerId && !composerUnsynced) composerDiscarded = true;
      continue;
    }
    // 已确认编辑的同步修订晚于丢弃水位：该正文晚于丢弃产生，必须保留。
    if (entry.revision >= watermark) continue;
    // 更新但尚未同步的正文不是宿主丢弃的对象，保留本地工作副本。
    if (!entry.synced) continue;
    buddyDrafts.forget(buddyId, sessionId);
    if (sessionId === composerId) composerDiscarded = true;
  }
  if (!composerDiscarded) return;
  // 不直接取当前快照的 chat.draft：该快照可能已属于下一会话，会把别的
  // 会话正文写回旧会话缓存。先清空并用新队列接管，恢复/对账随后决定正文。
  draftEditGeneration += 1;
  draftSync.cancel();
  draftSync = createDraftSync();
  draft.value = '';
}

function reconcileDraft(): void {
  if (snapshot.value?.revoked === true) {
    draftSync.cancel();
    draft.value = '';
    revokeArmed.value = false;
    return;
  }
  // 输入法组合期间本地文本仍在变化，不能拿宿主旧草稿覆盖。
  if (composing.value || !draftSync.isSynced()) return;
  const hostDraft = displayDraft(snapshot.value);
  if (draft.value !== hostDraft) draft.value = hostDraft;
  draftSync.adoptHostDraft(hostDraft);
  const id = activeSessionId.value;
  if (id !== null) {
    // 已与宿主对齐的正文记录本伙伴本会话的权威修订，后续丢弃水位不能清它。
    buddyDrafts.markSynced(
      buddyTarget.expectedBuddyId,
      id,
      hostDraft,
      draftSessionRevision(snapshot.value, id),
    );
  }
}

function syncModelForm(force: boolean): void {
  const settings = modelSettings.value;
  if (!force && modelFormDirty(settings, modelForm, activeBuddyPersona.value)) return;
  if (force) modelInteractionGeneration += 1;
  Object.assign(modelForm, modelFormFromSettings(settings, activeBuddyPersona.value));
}

function discardPanelDrafts(): void {
  managementInteractionGeneration += 1;
  modelInteractionGeneration += 1;
  const cleared = clearedSensitiveDrafts();
  modelForm.apiKeyInput = cleared.apiKeyInput;
  modelForm.clearApiKey = cleared.clearApiKey;
  Object.assign(modelForm, modelFormFromSettings(modelSettings.value, activeBuddyPersona.value));
  memoryEdit.value = cleared.memoryEdit;
  memoryDraft.value = '';
  rememberDraft.value = cleared.rememberDraft;
  memoryQuery.value = cleared.searchQuery;
  memoryNotice.value = null;
  memoryError.value = null;
  deleteArm.value = null;
  modelApplyNotice.value = null;
}

function resetSensitiveForRevoke(): void {
  activePanel.value = null;
  activePage.value = 'chat';
  pendingPanelLeave.value = null;
  pendingLeavePage.value = null;
  pendingLeaveLabel.value = null;
  panelGuard.cancel();
  panelNotice.value = null;
  resetPanelMemoryState();
  discardPanelDrafts();
  syncModelForm(true);
  // 撤权后伙伴注册表、全部伙伴的会话草稿与丢弃水位都不得继续留在客户端缓存。
  buddyDrafts.clear();
  buddyDiscardWatermarks.clear();
  buddyScopeGeneration += 1;
  const revokedBuddyId = buddyTarget.expectedBuddyId;
  buddyTarget = buddyExpectationOf(null);
  commandLock.migrateGlobalUnknown(revokedBuddyId, buddyTarget.expectedBuddyId);
  managementDraftSync.cancel();
  managementDraftSync = createManagementDraftSync();
  managementSyncRevision.value += 1;
  // 未知锁按伙伴作用域保留：撤权会清展示缓存，但不把未确认的外部副作用当成已收口。
  syncCommandUnconfirmed();
  buddyCreateOpen.value = false;
  buddyRenameOpen.value = false;
  buddyForm.label = '';
  buddyForm.persona = '';
  buddyRenameLabel.value = '';
  buddyNotice.value = null;
  buddyError.value = null;
  closeDialogBuddyId.value = null;
  archiveArmed.value = false;
  draftTarget = { expectedSessionId: null, expectedSessionsRevision: 0 };
  draftEditGeneration += 1;
  modelCommitIntent = null;
  buddyCommitIntent = null;
  draft.value = '';
  draftSync.cancel();
  lastManagementDraftSignal = managementDraftSignal.value;
}

/** 显式丢弃管理草稿时同时清空创建/改名表单，不让受保护文本残留。 */
function clearBuddyFormDrafts(): void {
  buddyCommitIntent = null;
  buddyCreateOpen.value = false;
  buddyRenameOpen.value = false;
  buddyForm.label = '';
  buddyForm.persona = '';
  buddyRenameLabel.value = '';
  buddyError.value = null;
}

/**
 * 权威管理丢弃回执命中的统一清理：面板与伙伴表单一起清。
 * 清场本身不是新编辑，因此把当前信号记为基线，避免再推进保护版本。
 */
function discardManagedDrafts(): void {
  discardPanelDrafts();
  clearBuddyFormDrafts();
  lastManagementDraftSignal = managementDraftSignal.value;
}

function resetPanelMemoryState(): void {
  memoryEdit.value = null;
  memoryDraft.value = '';
  rememberDraft.value = '';
  memoryQuery.value = '';
  memoryNotice.value = null;
  memoryError.value = null;
  deleteArm.value = null;
}

/**
 * 消费与本次显式模型保存精确匹配的成功回执：
 * - 动作、发起伙伴/修订与管理版本必须完全一致；
 * - 当前快照必须已经处于回执指定的新 context（同一伙伴、buddiesRevision 到达
 *   toRevision），避免无关迟到回执或后续动作被当成这次保存；
 * - 本地模型交互代际仍须符合发起值；期间有新编辑时只确认旧保存已结算，
 *   不清新编辑，也不回填旧配置；
 * - 密码输入和已提交表单基线正常清除并退出 dirty；丢失回包后由事件或重读
 *   带来的同一回执也能收口 Unknown。
 */
function consumeCommittedModelApply(current: PrototypeSnapshot | null): void {
  const intent = modelCommitIntent;
  if (intent === null || intent.applied || current === null || current.revoked) return;
  const receipt = current.buddyCommit;
  if (!buddyCommitMatchesIntent(intent, receipt) || receipt === null) return;
  // 证据只消费一次；即使当前编辑已更新，也不能让同一回执以后再次清稿。
  intent.applied = true;
  modelCommitIntent = null;

  // 该回执证明本次保存已完成，先按发起伙伴收口对应 Unknown。
  if (unconfirmedOperation === 'model') {
    commandLock.confirmScoped('model', intent.fromBuddyId, null);
    if (!commandLock.locked) unconfirmedOperation = null;
    syncCommandUnconfirmed();
  }

  // 之后到达的无关动作已推进 context，或用户已离开配置面板：只确认结算，
  // 不用旧回执覆盖当前表单。
  if (
    disposed ||
    revoked.value ||
    activePanel.value !== 'presets' ||
    activeBuddyId.value !== receipt.toBuddyId ||
    buddiesRevision.value !== receipt.toRevision
  ) {
    return;
  }
  // 发起后出现新编辑：旧保存已成功，但更新编辑仍受退出保护，不能被清掉。
  if (modelInteractionGeneration !== intent.interaction) return;

  Object.assign(
    modelForm,
    modelFormAfterApply(displayModelSettings(current), current.activeBuddyPersona),
  );
  modelApplyNotice.value = '模型配置已应用到桌面宿主。';
  // 表单回到宿主基线不是新编辑；先在本地基线中消费已提交版本，再同步 clean。
  lastManagementDraftSignal = managementDraftSignal.value;
  if (current.managementDraft.dirty || managementDraftSync.dirty) {
    managementDraftSync.record(false, 'none', buddyTarget);
  }
}

function applyResponse(
  response: SnapshotResponse,
  operation: Operation,
  capturedSessionId: string | null = null,
  capturedBuddyId: string = buddyTarget.expectedBuddyId,
): SnapshotUpdate {
  if (disposed) return { kind: 'none' };
  // 撤权是单调的：已撤权的展示不能被迟到、未撤权的业务回包复活。
  // 在写入排序存储前拒绝，避免旧非撤权快照占据更高展示修订。
  const previouslyRevoked = snapshots.current?.revoked === true;
  if (previouslyRevoked && response.snapshot !== null && response.snapshot.revoked !== true) {
    return { kind: 'none' };
  }
  // 伙伴作用域切换前的快照用于把旧伙伴草稿留在它自己的槽位。
  const previousSnapshot = snapshots.current;
  const update = snapshots.applyResponse(response);
  if (update.kind === 'none') {
    // 事件或较新的重读可能已应用同一回执；这里仍按同一证据收口，避免回复
    // 先到/丢失后重读两个入口不一致。
    consumeCommittedModelApply(snapshots.current);
    consumeCommittedBuddyAction(snapshots.current);
    return update;
  }

  // 管理编辑丢弃回执必须属于当前同步实例绑定的**完整作用域**；旧伙伴或同一
  // 伙伴的旧选择修订回执都不清当前编辑。响应缺少注册表时使用空哨兵。
  consumeManagementDiscardReceipt(
    response.snapshot,
    managementDraftSync,
    discardManagedDrafts,
    buddyExpectationOf(response.snapshot),
  );

  if (update.kind === 'success') {
    const wasInitialized = receivedSuccessfulSnapshot;
    snapshot.value = snapshots.current;
    receivedSuccessfulSnapshot = true;
    // 必须先按同一 buddyCommit 消费本次提交证据，再采纳作用域：事件先到时
    // adoptBuddyScope 不能先把提交后新增的表单正文清掉。
    consumeCommittedBuddyAction(snapshot.value);
    // 宿主注册表是伙伴身份与选择的唯一权威；切换时隔离全部伙伴作用域缓存。
    adoptBuddyScope(previousSnapshot);
    syncCloseDialogScope();
    if (!wasInitialized && snapshot.value !== null) {
      managementDraftSync.adopt(
        snapshot.value.managementDraft,
        buddyTarget,
      );
    }
    // 未知按 (伙伴, 会话) 隔离；旧伙伴/旧会话的未知仍保留在锁中。
    commandLock.enterScope(
      buddyTarget.expectedBuddyId,
      sessionCatalogOf(snapshot.value)?.activeSessionId ?? null,
    );
    consumeCommittedModelApply(snapshot.value);
    if (isSideEffectOperation(operation) && unconfirmedOperation === operation) {
      commandLock.confirmScoped(operation, capturedBuddyId, capturedSessionId);
      unconfirmedOperation = null;
    }
    syncCommandUnconfirmed();
    if (lastErrorRevision !== null && update.snapshot.revision >= lastErrorRevision) {
      clearError();
    }
    if (connectionState.value !== 'ready') {
      connectionState.value = 'ready';
      fatalMessage.value = '';
    }
    consumeChatDraftDiscard(snapshot.value);
    adoptSessionScope();
    reconcileDraft();
    // 撤权是单调的：即使响应快照因排序未覆盖当前展示，也立即清空敏感缓存。
    // 首个权威快照强制建立表单基线：未配置模型的伙伴也要显示自己已绑定的人设。
    if (snapshot.value?.revoked === true || update.snapshot.revoked === true) {
      resetSensitiveForRevoke();
    } else {
      syncModelForm(!wasInitialized);
    }
    return update;
  }

  if (update.stale) {
    consumeCommittedModelApply(snapshots.current);
    consumeCommittedBuddyAction(snapshots.current);
    return update;
  }
  snapshot.value = snapshots.current;
  consumeCommittedBuddyAction(snapshot.value);
  adoptBuddyScope(previousSnapshot);
  syncCloseDialogScope();
  commandLock.enterScope(
    buddyTarget.expectedBuddyId,
    sessionCatalogOf(snapshot.value)?.activeSessionId ?? null,
  );
  consumeCommittedModelApply(snapshot.value);
  consumeChatDraftDiscard(snapshot.value);
  adoptSessionScope();
  // 明确未执行的失败不能成为之后某次成功回包清除更早 Unknown 的凭据。
  if (
    isSideEffectOperation(operation) &&
    unconfirmedOperation === operation &&
    update.error.disposition === 'not_executed'
  ) {
    unconfirmedOperation = null;
  }
  if (
    isSideEffectOperation(operation) && update.error.disposition === 'unknown'
  ) {
    // 撤权后原伙伴作用域已关闭；把未知保留在关闭后的作用域，绝不把撤权当成已收口。
    markCommandUnknown(
      operation,
      'business_unknown',
      operation === 'send' ? capturedSessionId : null,
      snapshot.value?.revoked === true ? buddyTarget.expectedBuddyId : capturedBuddyId,
    );
  } else {
    syncCommandUnconfirmed();
  }
  setError(
    describeHostFailure(operation, update.error),
    update.snapshot === null ? null : update.snapshot.revision,
  );
  reconcileDraft();
  if (snapshot.value?.revoked === true) {
    resetSensitiveForRevoke();
  } else {
    syncModelForm(false);
  }
  return update;
}

function handleSnapshotEvent(response: SnapshotResponse | null): void {
  if (disposed) return;
  if (response === null) {
    setError('收到无法识别的桌面状态，已忽略。', null);
    return;
  }
  applyResponse(response, 'load');
}

function handleFrameEvent(state: MaterialFrameDescriptor | null): void {
  frameEventCount += 1;
  if (disposed || state === null) return;
  paused.value = state.paused;
}

async function refreshPausedFromHost(): Promise<void> {
  const seenAtRequest = frameEventCount;
  try {
    const frame = await requestFrame();
    if (!disposed && frameEventCount === seenAtRequest) paused.value = frame.paused;
  } catch {
    // 暂停徽标是可选的窗口状态；取不到时保留通用按钮文案，不影响聊天主链。
  }
}

function onDraftInput(event: Event): void {
  const target = event.target as HTMLTextAreaElement;
  const inputEvent = event as InputEvent;
  if (composing.value || inputEvent.isComposing) return;
  draft.value = target.value;
  draftEditGeneration += 1;
  rememberCurrentDraft(target.value);
  draftSync.enqueue(target.value);
}

function onCompositionStart(): void {
  composing.value = true;
}

function onCompositionEnd(event: CompositionEvent): void {
  composing.value = false;
  const target = event.target as HTMLTextAreaElement;
  draft.value = target.value;
  draftEditGeneration += 1;
  rememberCurrentDraft(target.value);
  draftSync.enqueue(target.value);
}

function handleKeydown(event: KeyboardEvent): void {
  const action = resolveComposerKey({
    key: event.key,
    shiftKey: event.shiftKey,
    composing: composing.value,
    eventComposing: event.isComposing,
    keyCode: event.keyCode,
    canSend: canSend.value,
  });
  if (action === 'send') {
    event.preventDefault();
    void send();
  }
}

async function send(): Promise<void> {
  if (busy.value || !canSend.value) return;
  const requestedSessionId = activeSessionId.value;
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  let expectation = sessionExpectation(snapshot.value);
  busy.value = true;
  clearError();
  try {
    if (!(await draftSync.flushLatest(draft.value))) {
      if (!disposed) setError('草稿尚未同步到桌面宿主，本次发送已取消。', null);
      return;
    }
    if (disposed) return;
    // 草稿同步期间的会话/伙伴变化必须取消本轮发送，绝不让旧预期进入新作用域。
    const latestBuddy = buddyExpectationOf(snapshots.current);
    if (!buddyExpectationMatches(latestBuddy, capturedBuddy)) {
      setError('伙伴在草稿同步期间发生变化，本次发送已取消；草稿已保留。', null);
      return;
    }
    const latest = sessionExpectation(snapshot.value);
    if (latest.expectedSessionId !== requestedSessionId) {
      setError('会话在草稿同步期间发生变化，本次发送已取消；草稿已保留。', null);
      return;
    }
    expectation = latest;
    rememberCurrentDraft(draft.value);
    const response = await dispatchPrototype(sendCommand(expectation), latestBuddy);
    if (disposed) return;
    if (responseBuddyMismatch(response, capturedBuddy.expectedBuddyId)) {
      setError('收到其他伙伴的发送回包，未回填当前伙伴；请重新读取状态确认结果。', null);
      return;
    }
    if (responseCatalogMismatch(response, expectation)) {
      // 不把另一会话的回包当作本轮结果，也不清对应会话锁。
      setError('收到其他会话的发送回包，未回填当前会话；请重新读取状态确认结果。', null);
      return;
    }
    if (sessionResponseIsStale(response, expectation)) {
      // 同 ID 但目录修订较旧的回包不能回写当前目录或草稿；请求本身不重发。
      setError('收到较旧会话目录修订的发送回包，未回填当前会话；请重新读取状态确认结果。', null);
      return;
    }
    applyResponse(
      response,
      'send',
      expectation.expectedSessionId,
      capturedBuddy.expectedBuddyId,
    );
  } catch (error) {
    if (!disposed) {
      markCommandUnknown(
        'send',
        'ipc_reply_lost',
        expectation.expectedSessionId,
        capturedBuddy.expectedBuddyId,
      );
      setError(describeThrownFailure('send', error), null);
    }
  } finally {
    if (!disposed) {
      busy.value = false;
      void nextTick(() => inputRef.value?.focus());
    }
  }
}

function sessionLabel(session: SessionView): string {
  const label = session.label.trim();
  return label.length > 0 ? label : session.id;
}

function sessionTone(session: SessionView): string {
  if (session.archived) return 'tone-muted';
  if (session.status === 'unknown') return 'tone-warn';
  if (session.status === 'failed') return 'tone-danger';
  if (session.status === 'sending') return 'tone-busy';
  return 'tone-ok';
}

/** 会话目录修订变旧时，迟到回包不得改写当前草稿或选择；撤权快照必须进入清理路径。 */
function sessionResponseIsStale(
  response: SnapshotResponse,
  expectation: SessionExpectation,
): boolean {
  if (response.snapshot?.revoked === true) return false;
  const catalog = sessionCatalogOf(response.snapshot);
  if (catalog === null) return false;
  return catalog.sessionsRevision < expectation.expectedSessionsRevision;
}

/** 成功回包也必须符合本次动作语义，否则如实报未确认，不伪造会话。 */
function sessionActionOutcomeProblem(
  operation: 'new_session' | 'switch_session' | 'archive_session',
  targetId: string | null,
  previousSessionId: string | null,
): string | null {
  const catalog = sessionCatalogOf(snapshots.current);
  if (catalog === null) return null;
  if (operation === 'new_session') {
    if (catalog.activeSessionId === null) {
      return '桌面宿主未签发新的会话目录项，未确认本次新建。';
    }
    if (catalog.activeSessionId === previousSessionId) {
      return '桌面宿主未切换到新会话，未确认本次新建。';
    }
    return null;
  }
  if (operation === 'switch_session') {
    return catalog.activeSessionId === targetId
      ? null
      : '桌面宿主未切换到所选会话，请重新读取状态后再操作。';
  }
  if (catalog.activeSessionId !== previousSessionId) {
    return '归档后当前会话已变化，请重新读取状态。';
  }
  const target = catalog.sessions.find((session) => session.id === targetId);
  if (target === undefined || !target.archived) {
    return '桌面宿主未确认当前会话已归档，请重新读取状态。';
  }
  return null;
}

async function runSessionAction(
  operation: 'new_session' | 'switch_session' | 'archive_session',
  targetId: string | null,
  build: (expectation: SessionExpectation) => ReturnType<typeof newSessionCommand>,
): Promise<void> {
  if (busy.value) return;
  const block =
    operation === 'new_session'
      ? newSessionBlockReason.value
      : operation === 'archive_session'
        ? archiveBlockReason.value
        : switchSessionBlockReason.value;
  if (block !== null) {
    setError(block, null);
    return;
  }
  const expectationBeforeFlush = sessionExpectation(snapshot.value);
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  const previousSessionId = expectationBeforeFlush.expectedSessionId;
  let previousDraft = draft.value;
  if (previousSessionId !== null) rememberSessionDraft(previousSessionId, previousDraft);

  // 先完成当前草稿同步，再请求生命周期动作；失败就取消并保留草稿。
  if (!activeSessionArchived.value) {
    const synced = await draftSync.flushLatest(draft.value);
    if (disposed) return;
    if (!synced) {
      setError('草稿尚未同步到桌面宿主，本次会话操作已取消；草稿已保留。', null);
      return;
    }
  }
  if (disposed) return;

  // 同步期间的伙伴变化必须取消本轮会话动作，伙伴上下文不能替代 A 的校验。
  const latestBuddy = buddyExpectationOf(snapshots.current);
  if (!buddyExpectationMatches(latestBuddy, capturedBuddy)) {
    setError('伙伴在草稿同步期间发生变化，本次会话操作已取消；草稿已保留。', null);
    return;
  }
  // 同步期间用户仍可输入；以同步完成后的最新正文与会话预期为准。
  const expectation = sessionExpectation(snapshot.value);
  if (expectation.expectedSessionId !== previousSessionId) {
    setError('会话在草稿同步期间发生变化，本次会话操作已取消；草稿已保留。', null);
    return;
  }
  previousDraft = draft.value;
  if (previousSessionId !== null) rememberSessionDraft(previousSessionId, previousDraft, true);
  // 派发凭据固定在这一刻：之后任何会话/修订/编辑变化都会让失败回包失去恢复资格。
  const capture: SessionDraftCapture = {
    expectedSessionId: expectation.expectedSessionId,
    expectedSessionsRevision: expectation.expectedSessionsRevision,
    draftEditGeneration,
  };

  busy.value = true;
  clearError();
  try {
    const response = await dispatchPrototype(build(expectation), latestBuddy);
    if (disposed) return;
    // 撤权回包必须先进统一清理路径；不能被“目录较旧”或“会话不匹配”提前丢弃。
    if (response.snapshot?.revoked === true) {
      applyResponse(response, operation, previousSessionId, capturedBuddy.expectedBuddyId);
      return;
    }
    if (responseBuddyMismatch(response, capturedBuddy.expectedBuddyId)) {
      setError('收到其他伙伴的会话回包，已忽略本次结果；草稿与选择保持原样。', null);
      return;
    }
    if (sessionResponseIsStale(response, expectation)) {
      setError('收到较旧的会话目录回包，已忽略本次结果；草稿与选择保持原样。', null);
      return;
    }
    const update = applyResponse(
      response,
      operation,
      previousSessionId,
      capturedBuddy.expectedBuddyId,
    );
    if (update.kind === 'failure' && !update.stale) {
      // 拒绝/失败不清稿：只在会话、目录修订与编辑代际都未变时恢复原会话草稿。
      const stillPrevious = currentSessionStill(capture);
      if (stillPrevious) {
        if (previousSessionId !== null) rememberSessionDraft(previousSessionId, previousDraft, true);
        if (!disposed) draft.value = previousDraft;
      }
      return;
    }
    if (update.kind !== 'success') return;
    const problem = sessionActionOutcomeProblem(operation, targetId, previousSessionId);
    if (problem !== null) {
      if (currentSessionStill(capture) && previousSessionId !== null) {
        rememberSessionDraft(previousSessionId, previousDraft, true);
      }
      setError(problem, snapshots.current?.revision ?? null);
      return;
    }
    archiveArmed.value = false;
  } catch (error) {
    if (!disposed) {
      // 会话生命周期回包丢失时无法确定目录变化，按全局未知保守锁定；不自动重发。
      markCommandUnknown(
        operation,
        'ipc_reply_lost',
        null,
        capturedBuddy.expectedBuddyId,
      );
      setError(describeThrownFailure(operation, error), null);
      if (currentSessionStill(capture)) {
        if (previousSessionId !== null) rememberSessionDraft(previousSessionId, previousDraft, true);
        draft.value = previousDraft;
      }
    }
  } finally {
    if (!disposed) {
      busy.value = false;
      void nextTick(() => inputRef.value?.focus());
    }
  }
}

async function startNewSession(): Promise<void> {
  await runSessionAction('new_session', null, (expectation) => newSessionCommand(expectation));
}

async function switchToSession(id: string): Promise<void> {
  if (id === activeSessionId.value) return;
  await runSessionAction('switch_session', id, (expectation) =>
    switchSessionCommand(id, expectation),
  );
}

function requestArchiveSession(): void {
  if (busy.value || archiveBlockReason.value !== null) return;
  archiveArmed.value = true;
}

async function confirmArchiveSession(): Promise<void> {
  const id = activeSessionId.value;
  if (id === null) {
    archiveArmed.value = false;
    return;
  }
  archiveArmed.value = false;
  await runSessionAction('archive_session', id, (expectation) =>
    archiveSessionCommand(id, expectation),
  );
}

/** 显式整应用退出；与托盘共用宿主的全伙伴、全会话及管理稿确认链。 */
async function requestQuit(): Promise<void> {
  if (disposed || snapshot.value === null || interactionLocked.value) return;
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  const wasRevoked = revoked.value;
  const capturedDraft = draftSync;
  const capturedManagement = managementDraftSync;
  busy.value = true;
  clearError();
  try {
    // 先让本次输入触发的管理稿 watcher 登记版本，再等待宿主确认。
    await nextTick();
    if (!wasRevoked) {
      if (!(await capturedManagement.flush()) || !(await capturedDraft.flushLatest(draft.value))) {
        if (!disposed) setError('未保存内容尚未同步，退出已取消；请确认状态后重试。', null);
        return;
      }
    }
    if (disposed) return;
    if (
      !buddyExpectationMatches(buddyExpectationOf(snapshots.current), capturedBuddy) ||
      revoked.value !== wasRevoked ||
      capturedDraft !== draftSync || capturedManagement !== managementDraftSync ||
      (!wasRevoked && (!capturedManagement.isSynced || !capturedDraft.isSynced() ||
        capturedDraft.latestRequested !== draft.value))
    ) {
      setError('伙伴或未保存内容在同步期间变化，退出已取消；请重新操作。', null);
      return;
    }
    await invokeWindowCommand('quit', capturedBuddy);
  } catch {
    if (!disposed) setError('退出结果尚未确认。请检查退出提示或只读刷新状态，不会自动重试。', null);
  } finally {
    if (!disposed) busy.value = false;
  }
}

async function requestClose(): Promise<void> {
  if (busy.value || revoked.value || closeConfirmation.value) return;
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  busy.value = true;
  clearError();
  try {
    if (!(await managementDraftSync.flush())) {
      if (!disposed) setError('管理编辑状态尚未同步到桌面宿主，本次关闭已取消。', null);
      return;
    }
    if (!(await draftSync.flushLatest(draft.value))) {
      if (!disposed) setError('草稿尚未同步到桌面宿主，本次关闭操作已取消。', null);
      return;
    }
    if (disposed) return;
    const latestBuddy = buddyExpectationOf(snapshots.current);
    if (!buddyExpectationMatches(latestBuddy, capturedBuddy)) {
      setError('伙伴在同步期间发生变化，本次关闭操作已取消。', null);
      return;
    }
    applyResponse(
      await dispatchPrototype(closeChatCommand(), latestBuddy),
      'close',
      null,
      capturedBuddy.expectedBuddyId,
    );
  } catch (error) {
    if (!disposed) {
      markCommandUnknown('close', 'ipc_reply_lost', null, capturedBuddy.expectedBuddyId);
      setError(describeThrownFailure('close', error), null);
    }
  } finally {
    if (!disposed) busy.value = false;
  }
}

async function resolveClose(discard: boolean): Promise<void> {
  if (busy.value) return;
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  // 关闭确认绑定发起时的伙伴：切换后旧确认不得作用于新伙伴。
  if (closeDialogBuddyId.value !== capturedBuddy.expectedBuddyId) {
    setError('关闭确认所属伙伴已变化，本次确认未执行；请重新读取状态。', null);
    return;
  }
  busy.value = true;
  clearError();
  try {
    applyResponse(
      await dispatchPrototype(
        confirmCloseCommand(discard, snapshot.value?.closeIntent ?? null),
        capturedBuddy,
      ),
      'confirm_close',
      null,
      capturedBuddy.expectedBuddyId,
    );
  } catch (error) {
    if (!disposed) {
      markCommandUnknown(
        'confirm_close',
        'ipc_reply_lost',
        null,
        capturedBuddy.expectedBuddyId,
      );
      setError(describeThrownFailure('confirm_close', error), null);
    }
  } finally {
    if (!disposed) busy.value = false;
  }
}

async function resolveManagementExit(discard: boolean): Promise<void> {
  if (busy.value) return;
  const exit = managementExitConfirmation.value;
  if (exit === null) return;
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  if (!managementDraftSync.matchesScope(capturedBuddy)) {
    setError('管理编辑状态所属伙伴作用域已变化，本次退出处理未执行；请重新读取状态。', null);
    return;
  }
  busy.value = true;
  clearError();
  try {
    if (!(await managementDraftSync.flush()) || managementDraftSync.version !== exit.version) {
      setError('管理编辑已变化或尚未同步，请等待宿主确认后再处理退出。', null);
      return;
    }
    const command = discard
      ? confirmManagementDiscardCommand(exit.version, exit.intent)
      : cancelManagementExitCommand(exit.version, exit.intent);
    applyResponse(
      await dispatchPrototype(command, capturedBuddy),
      'management_exit',
      null,
      capturedBuddy.expectedBuddyId,
    );
  } catch (error) {
    if (!disposed) {
      markCommandUnknown(
        'management_exit',
        'ipc_reply_lost',
        null,
        capturedBuddy.expectedBuddyId,
      );
      setError(describeThrownFailure('management_exit', error), null);
    }
  } finally {
    if (!disposed) busy.value = false;
  }
}

function requestRevoke(): void {
  if (canRequestRevoke.value) revokeArmed.value = true;
}

async function confirmRevoke(): Promise<void> {
  if (busy.value) return;
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  busy.value = true;
  clearError();
  // 撤权前丢弃尚未派发的草稿，避免撤权后仍有 set_draft 排队发出。
  draftSync.discardPending();
  try {
    applyResponse(
      await dispatchPrototype(revokeCommand(), capturedBuddy),
      'revoke',
      null,
      capturedBuddy.expectedBuddyId,
    );
    if (snapshot.value?.revoked === true) {
      revokeArmed.value = false;
      return;
    }
    if (!disposed) {
      draft.value = displayDraft(snapshot.value);
      if (!draftSync.hasPendingWork) draftSync.adoptHostDraft(draft.value);
    }
  } catch (error) {
    if (!disposed) {
      markCommandUnknown(
        'revoke',
        'ipc_reply_lost',
        null,
        capturedBuddy.expectedBuddyId,
      );
      setError(describeThrownFailure('revoke', error), null);
      draft.value = displayDraft(snapshot.value);
    }
  } finally {
    if (!disposed) busy.value = false;
  }
}

/**
 * 页面导航的前置说明；为 null 才允许发起。面板页需要 Hide/Open IPC，
 * 纯呈现页在没有面板切换时可直接前往，不受面板许可影响。
 */
function pageNavBlockReason(page: PageId): string | null {
  if (page === activePage.value && PAGE_PANELS[page] === activePanel.value) return null;
  if (pageNavigating.value || panelBusy.value) {
    return '上一次页面切换尚未完成，请等待当前导航结束。';
  }
  if (closeConfirmation.value || managementExitConfirmation.value !== null || revokeArmed.value) {
    return '关闭/退出确认处理中，暂不能切换页面。';
  }
  const targetPanel = PAGE_PANELS[page];
  const opensPanel = targetPanel !== null && activePanel.value !== targetPanel;
  const leavesPanel = activePanel.value !== null && activePanel.value !== targetPanel;
  if ((opensPanel || leavesPanel) && !panelActionsAllowed.value) {
    if (revoked.value) return '授权已撤回，没有可用的管理面板。';
    if (connectionState.value !== 'ready' || snapshot.value === null) {
      return '尚未连接桌面宿主，暂不能切换管理页面。';
    }
    return '桌面宿主确认未完成，暂不能打开或关闭管理面板。';
  }
  return null;
}

/**
 * 提交页面选择：涉及面板时等待 Hide/Open 成功回执，成功才切换；
 * 管理稿未保存时先经离开确认，取消不丢稿也不换页。
 */
function requestPage(target: PageId): void {
  if (target === activePage.value && PAGE_PANELS[target] === activePanel.value) return;
  if (pageNavBlockReason(target) !== null) return;
  const targetPanel = PAGE_PANELS[target];
  const leavingDirty =
    activePanel.value !== null && activePanel.value !== targetPanel && panelDirty.value;
  if (!panelGuard.request(targetPanel ?? 'close', leavingDirty)) {
    pendingPanelLeave.value = panelGuard.pending;
    pendingLeavePage.value = target;
    pendingLeaveLabel.value = PAGES[target].name;
    return;
  }
  void performPageTransition(target);
}

function cancelPanelLeave(): void {
  panelGuard.cancel();
  pendingPanelLeave.value = null;
  pendingLeavePage.value = null;
  pendingLeaveLabel.value = null;
}

function confirmPanelLeave(): void {
  const target = panelGuard.confirm();
  const page = pendingLeavePage.value;
  pendingPanelLeave.value = null;
  pendingLeavePage.value = null;
  pendingLeaveLabel.value = null;
  if (target === null) return;
  if (page !== null) {
    void performPageTransition(page);
    return;
  }
  void performPanelAction(target);
}

/**
 * 页面切换 = 必要的面板 Hide/Open 序列。整串成功才提交目标页面；
 * Hide 成功但 Open 失败时面板已关闭，回退到聊天首页，错误横幅跨页可见。
 */
async function performPageTransition(target: PageId): Promise<void> {
  pageNavigating.value = true;
  try {
    const targetPanel = PAGE_PANELS[target];
    // 没有业务面板需要切换时只改变呈现；不清错误、不处理未知结果或模拟回执。
    const ok = activePanel.value === null && targetPanel === null
      ? true
      : await performPanelAction(targetPanel ?? 'close');
    if (disposed) return;
    if (ok) {
      activePage.value = target;
      void nextTick(() => focusPageHeading(target));
      if (target === 'chat') scrollToBottom();
      return;
    }
    if (PAGE_PANELS[activePage.value] !== null && activePanel.value === null) {
      activePage.value = 'chat';
    }
  } finally {
    pageNavigating.value = false;
  }
}

/** 切页后把焦点移到页面标题，键盘与读屏用户能感知页面变化。 */
function focusPageHeading(page: PageId): void {
  // 组件回归在无 DOM 的 VM 中执行同一脚本；没有文档时跳过焦点管理。
  if (typeof document === 'undefined') return;
  const heading = document.getElementById(`page-${page}-title`);
  if (heading !== null) {
    heading.tabIndex = -1;
    heading.focus({ preventScroll: true });
  }
}

/** 面板动作按成功回执推进；隐藏成功才清面板，打开成功才登记新面板。 */
async function performPanelAction(target: PanelTarget): Promise<boolean> {
  panelBusy.value = true;
  panelNotice.value = null;
  clearError();
  // 面板动作在发起时捕获伙伴上下文；切换伙伴会由挂起队列与 busy 阻止。
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  try {
    const current = activePanel.value;
    if (current !== null) {
      const hidden = applyResponse(
        await dispatchPrototype(hidePanelCommand(current), capturedBuddy),
        'panel',
        null,
        capturedBuddy.expectedBuddyId,
      );
      if (disposed || hidden.kind !== 'success' || revoked.value) return false;
      activePanel.value = null;
      discardPanelDrafts();
    }
    if (target === 'close') {
      return true;
    }
    const update = applyResponse(
      await dispatchPrototype(openPanelCommand(target), capturedBuddy),
      'panel',
      null,
      capturedBuddy.expectedBuddyId,
    );
    if (!disposed && update.kind === 'success' && snapshot.value?.revoked !== true) {
      activePanel.value = target;
      if (target === 'presets') syncModelForm(true);
      if (target === 'memory') {
        memoryNotice.value = null;
        memoryError.value = null;
      }
      return true;
    }
    return false;
  } catch (error) {
    if (!disposed && !revoked.value) setError(describeThrownFailure('panel', error), null);
    return false;
  } finally {
    if (!disposed) panelBusy.value = false;
  }
}

function onModelIdentityInput(): void {
  noteModelInteraction();
  if (requiresTransmissionReset(modelSettings.value, modelForm)) {
    modelForm.allowTransmission = false;
    modelForm.transmissionTouched = false;
    modelApplyNotice.value = '端点或模型已变化，发送许可已取消；请重新确认后再应用。';
  }
}

function onTransmissionChange(): void {
  noteModelInteraction();
  modelForm.transmissionTouched = true;
}

function noteModelInteraction(): void {
  modelInteractionGeneration += 1;
}

function noteManagementInteraction(): void {
  managementInteractionGeneration += 1;
}

function resetModelForm(): void {
  modelInteractionGeneration += 1;
  Object.assign(modelForm, modelFormFromSettings(modelSettings.value, activeBuddyPersona.value));
  modelApplyNotice.value = null;
}

async function applyModelSettings(): Promise<void> {
  if (!canMutate.value) return;
  const built = buildConfigureModelCommand(modelSettings.value, modelForm);
  if (!built.ok) {
    modelApplyNotice.value = built.message;
    return;
  }
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  const capturedScopeGeneration = buddyScopeGeneration;
  panelBusy.value = true;
  modelInteractionGeneration += 1;
  const interaction = modelInteractionGeneration;
  modelApplyNotice.value = null;
  clearError();
  let commit: ModelCommitIntent | null = null;
  try {
    // 提交前把当前表单对应的 dirty/种类精确同步到宿主，并等待确认；回执中的
    // managementVersion 才能与本次发起证据严格对上。显式点击保存即使表单
    // 没有新字符变化，也作为当前 context 下的新用户意图重新确认一次。
    const desired = managementDraftIntent.value;
    if (desired.dirty && (!managementDraftSync.dirty || managementDraftSync.kind !== desired.kind)) {
      if (!managementDraftSync.record(desired.dirty, desired.kind, buddyTarget)) {
        modelApplyNotice.value = '管理编辑版本已达到上限，本次保存未派发。';
        return;
      }
      lastManagementDraftSignal = managementDraftSignal.value;
    }
    if (!(await managementDraftSync.flush())) {
      if (!disposed) {
        modelApplyNotice.value = '管理编辑状态尚未同步到桌面宿主，本次保存未派发。';
      }
      return;
    }
    if (disposed) return;
    if (
      modelInteractionGeneration !== interaction ||
      buddyScopeGeneration !== capturedScopeGeneration ||
      !buddyExpectationMatches(buddyExpectationOf(snapshots.current), capturedBuddy) ||
      activePanel.value !== 'presets'
    ) {
      modelApplyNotice.value = '表单或伙伴上下文在准备保存期间发生变化；请重新点击应用配置。';
      return;
    }
    commit = {
      kind: 'configure_model',
      fromBuddyId: capturedBuddy.expectedBuddyId,
      fromRevision: capturedBuddy.expectedBuddiesRevision,
      managementVersion: managementDraftSync.version,
      interaction,
      applied: false,
    };
    modelCommitIntent = commit;
    const response = await dispatchPrototype(built.command, capturedBuddy);
    if (disposed) return;
    const update = applyResponse(response, 'model', null, capturedBuddy.expectedBuddyId);
    // 明确未执行/已结算的失败不可能再产生匹配成功回执；保留 Unknown 供重读收口。
    if (
      update.kind === 'failure' &&
      !update.stale &&
      update.error.disposition !== 'unknown' &&
      modelCommitIntent === commit
    ) {
      modelCommitIntent = null;
    }
  } catch (error) {
    if (!disposed) {
      if (commit !== null && commit.applied) {
        // 事件或匹配回执已经证明本次保存完成；IPC 回复丢失不再是未知。
        return;
      }
      if (!revoked.value) {
        markCommandUnknown(
          'model',
          'ipc_reply_lost',
          null,
          capturedBuddy.expectedBuddyId,
        );
        setError(describeThrownFailure('model', error), null);
      }
    }
  } finally {
    if (!disposed) panelBusy.value = false;
  }
}

async function selectPreset(preset: PresetView): Promise<void> {
  if (!canMutate.value || preset.id === activePreset.value) return;
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  const scopeGeneration = buddyScopeGeneration;
  panelBusy.value = true;
  const interaction = managementInteractionGeneration;
  panelNotice.value = null;
  clearError();
  try {
    const update = applyResponse(
      await dispatchPrototype(selectPresetCommand(preset.id), capturedBuddy),
      'preset',
      null,
      capturedBuddy.expectedBuddyId,
    );
    if (!isManagementInteractionCurrent(interaction, 'presets', scopeGeneration)) return;
    if (update.kind === 'success') {
      panelNotice.value = `已切换预设：${preset.label.length > 0 ? preset.label : preset.id}`;
    }
  } catch (error) {
    if (isManagementInteractionCurrent(interaction, 'presets', scopeGeneration)) {
      markCommandUnknown(
        'preset',
        'ipc_reply_lost',
        null,
        capturedBuddy.expectedBuddyId,
      );
      setError(describeThrownFailure('preset', error), null);
    }
  } finally {
    if (!disposed) panelBusy.value = false;
  }
}

async function searchMemories(): Promise<void> {
  if (!panelActionsAllowed.value || panelBusy.value) return;
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  const scopeGeneration = buddyScopeGeneration;
  panelBusy.value = true;
  const interaction = managementInteractionGeneration;
  memoryError.value = null;
  clearError();
  try {
    const update = applyResponse(
      await dispatchPrototype(searchMemoriesCommand(memoryQuery.value), capturedBuddy),
      'memory_search',
      null,
      capturedBuddy.expectedBuddyId,
    );
    if (!isManagementInteractionCurrent(interaction, 'memory', scopeGeneration)) return;
    if (update.kind === 'failure' && !update.stale) {
      memoryError.value = describeHostFailure('memory_search', update.error);
    }
  } catch (error) {
    if (isManagementInteractionCurrent(interaction, 'memory', scopeGeneration)) {
      memoryError.value = describeThrownFailure('memory_search', error);
      setError(memoryError.value, null);
    }
  } finally {
    if (!disposed) panelBusy.value = false;
  }
}

async function refreshMemories(): Promise<void> {
  if (!panelActionsAllowed.value || panelBusy.value) return;
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  const scopeGeneration = buddyScopeGeneration;
  panelBusy.value = true;
  const interaction = managementInteractionGeneration;
  memoryError.value = null;
  clearError();
  try {
    const update = applyResponse(
      await dispatchPrototype(refreshMemoriesCommand(), capturedBuddy),
      'memory_refresh',
      null,
      capturedBuddy.expectedBuddyId,
    );
    if (!isManagementInteractionCurrent(interaction, 'memory', scopeGeneration)) return;
    if (update.kind === 'failure' && !update.stale) {
      memoryError.value = describeHostFailure('memory_refresh', update.error);
    }
  } catch (error) {
    if (isManagementInteractionCurrent(interaction, 'memory', scopeGeneration)) {
      memoryError.value = describeThrownFailure('memory_refresh', error);
      setError(memoryError.value, null);
    }
  } finally {
    if (!disposed) panelBusy.value = false;
  }
}

async function rememberMemory(): Promise<void> {
  if (!canMutate.value) return;
  if (rememberDraft.value.trim().length === 0) {
    memoryError.value = '请先输入要记住的内容。';
    return;
  }
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  const scopeGeneration = buddyScopeGeneration;
  panelBusy.value = true;
  const interaction = managementInteractionGeneration;
  memoryError.value = null;
  memoryNotice.value = null;
  clearError();
  try {
    const update = applyResponse(
      await dispatchPrototype(rememberCommand(rememberDraft.value), capturedBuddy),
      'remember',
      null,
      capturedBuddy.expectedBuddyId,
    );
    if (!isManagementInteractionCurrent(interaction, 'memory', scopeGeneration)) return;
    if (update.kind === 'success') {
      managementInteractionGeneration += 1;
      rememberDraft.value = '';
      memoryNotice.value = '已写入一条私密记忆。';
    } else if (update.kind === 'failure') {
      memoryError.value = update.stale
        ? '收到较旧的宿主响应，已忽略；输入草稿已保留。'
        : describeHostFailure('remember', update.error);
    }
  } catch (error) {
    if (isManagementInteractionCurrent(interaction, 'memory', scopeGeneration)) {
      markCommandUnknown(
        'remember',
        'ipc_reply_lost',
        null,
        capturedBuddy.expectedBuddyId,
      );
      memoryError.value = describeThrownFailure('remember', error);
      setError(describeThrownFailure('remember', error), null);
    }
  } finally {
    if (!disposed) panelBusy.value = false;
  }
}

function beginEditMemory(memory: MemoryView): void {
  if (!canMutate.value) return;
  managementInteractionGeneration += 1;
  memoryEdit.value = beginMemoryEdit(memory);
  memoryDraft.value = memory.text;
  deleteArm.value = null;
  memoryError.value = null;
  memoryNotice.value = null;
}

function cancelMemoryEdit(): void {
  managementInteractionGeneration += 1;
  memoryEdit.value = null;
  memoryDraft.value = '';
  memoryError.value = null;
}

async function saveMemory(): Promise<void> {
  const edit = memoryEdit.value;
  if (edit === null || !canMutate.value) return;
  if (memoryDraft.value.trim().length === 0) {
    memoryError.value = '记忆内容不能为空。';
    return;
  }
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  const scopeGeneration = buddyScopeGeneration;
  panelBusy.value = true;
  const interaction = managementInteractionGeneration;
  const draftText = memoryDraft.value;
  memoryError.value = null;
  memoryNotice.value = null;
  clearError();
  try {
    const update = applyResponse(
      await dispatchPrototype(
        updateMemoryCommand(edit.id, edit.revision, draftText),
        capturedBuddy,
      ),
      'memory_update',
      null,
      capturedBuddy.expectedBuddyId,
    );
    if (
      !isManagementInteractionCurrent(interaction, 'memory', scopeGeneration) ||
      memoryEdit.value !== edit
    ) {
      return;
    }
    if (update.kind === 'success') {
      managementInteractionGeneration += 1;
      memoryEdit.value = null;
      memoryDraft.value = '';
      memoryNotice.value = '已保存为新的记忆版本。';
    } else if (update.kind === 'failure') {
      // 版本冲突、普通失败或乱序旧响应都按原目标与草稿恢复；不自动重试、不自行改写 revision。
      if (update.stale) {
        memoryError.value = '收到较旧的宿主响应，已忽略；编辑草稿已保留。';
      } else {
        const retained = retainMemoryDraft(edit, draftText, update.error);
        memoryEdit.value = retained.edit;
        memoryDraft.value = retained.draft;
        memoryError.value = retained.message;
      }
    }
  } catch (error) {
    if (
      isManagementInteractionCurrent(interaction, 'memory', scopeGeneration) &&
      memoryEdit.value === edit
    ) {
      markCommandUnknown(
        'memory_update',
        'ipc_reply_lost',
        null,
        capturedBuddy.expectedBuddyId,
      );
      memoryError.value = describeThrownFailure('memory_update', error);
      setError(describeThrownFailure('memory_update', error), null);
    }
  } finally {
    if (!disposed && !revoked.value) panelBusy.value = false;
  }
}

function isManagementInteractionCurrent(
  interaction: number,
  panel: PanelName,
  scopeGeneration: number = buddyScopeGeneration,
): boolean {
  return (
    !disposed &&
    !revoked.value &&
    activePanel.value === panel &&
    managementInteractionGeneration === interaction &&
    buddyScopeGeneration === scopeGeneration
  );
}

function requestDeleteMemory(memory: MemoryView): void {
  if (!canMutate.value) return;
  managementInteractionGeneration += 1;
  deleteArm.value = armMemoryDelete(memory);
  memoryNotice.value = null;
  memoryError.value = null;
}

function cancelDeleteMemory(): void {
  managementInteractionGeneration += 1;
  deleteArm.value = null;
}

async function confirmDeleteMemory(memory: MemoryView): Promise<void> {
  if (!canMutate.value) return;
  if (!confirmMemoryDelete(deleteArm.value, memory.id, memory.revision)) {
    deleteArm.value = null;
    memoryError.value = '这条记忆的版本已变化；请刷新列表后重新确认删除。';
    return;
  }
  const interaction = ++managementInteractionGeneration;
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  const scopeGeneration = buddyScopeGeneration;
  deleteArm.value = null;
  panelBusy.value = true;
  memoryError.value = null;
  memoryNotice.value = null;
  clearError();
  try {
    const update = applyResponse(
      await dispatchPrototype(
        deleteMemoryCommand(memory.id, memory.revision),
        capturedBuddy,
      ),
      'memory_delete',
      null,
      capturedBuddy.expectedBuddyId,
    );
    if (!isManagementInteractionCurrent(interaction, 'memory', scopeGeneration)) return;
    if (update.kind === 'success') {
      if (memoryEdit.value?.id === memory.id) {
        managementInteractionGeneration += 1;
        memoryEdit.value = null;
        memoryDraft.value = '';
      }
      memoryNotice.value = '已删除该记忆。';
    } else if (update.kind === 'failure') {
      memoryError.value = update.stale
        ? '收到较旧的宿主响应，已忽略；请重新确认删除。'
        : memoryDeleteFailureText(update.error);
    }
  } catch (error) {
    if (isManagementInteractionCurrent(interaction, 'memory', scopeGeneration)) {
      markCommandUnknown(
        'memory_delete',
        'ipc_reply_lost',
        null,
        capturedBuddy.expectedBuddyId,
      );
      memoryError.value = describeThrownFailure('memory_delete', error);
      setError(describeThrownFailure('memory_delete', error), null);
    }
  } finally {
    if (!disposed && !revoked.value) panelBusy.value = false;
  }
}

async function refreshDiagnostics(): Promise<void> {
  if (!panelActionsAllowed.value || panelBusy.value) return;
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  const scopeGeneration = buddyScopeGeneration;
  panelBusy.value = true;
  panelNotice.value = null;
  clearError();
  try {
    const update = applyResponse(
      await dispatchPrototype(refreshDiagnosticsCommand(), capturedBuddy),
      'diagnostics_refresh',
      null,
      capturedBuddy.expectedBuddyId,
    );
    if (
      !disposed &&
      !revoked.value &&
      buddyScopeGeneration === scopeGeneration &&
      activePanel.value === 'diagnostics' &&
      update.kind === 'success'
    ) {
      panelNotice.value = '诊断列表已刷新。';
    }
  } catch (error) {
    if (
      !disposed &&
      !revoked.value &&
      buddyScopeGeneration === scopeGeneration &&
      activePanel.value === 'diagnostics'
    ) {
      setError(describeThrownFailure('diagnostics_refresh', error), null);
    }
  } finally {
    if (!disposed) panelBusy.value = false;
  }
}

/**
 * 伙伴动作的恢复凭据：派发时的伙伴、注册表修订、作用域代际与本地编辑代际。
 * 只比较 ID 不够：同 ID 新修订、切换后继续编辑、撤权都必须拒绝恢复旧稿。
 */
interface BuddyActionCapture {
  expectedBuddyId: string;
  expectedBuddiesRevision: number;
  generation: number;
  draftEditGeneration: number;
}

function currentBuddyStill(capture: BuddyActionCapture): boolean {
  if (disposed || snapshots.current?.revoked === true) return false;
  if (buddyScopeGeneration !== capture.generation) return false;
  if (draftEditGeneration !== capture.draftEditGeneration) return false;
  const current = buddyExpectationOf(snapshots.current);
  // 只比较伙伴 ID 不够：同 ID 的选择修订变化或 A→B→A 都必须拒绝旧稿恢复。
  return (
    current.expectedBuddyId === capture.expectedBuddyId &&
    current.expectedBuddiesRevision === capture.expectedBuddiesRevision
  );
}

/** 成功回包也必须符合本次动作语义，否则如实报未确认，不伪造选择或身份。 */
function buddyActionOutcomeProblem(
  operation: 'create_buddy' | 'switch_buddy' | 'rename_buddy',
  targetId: string | null,
  previousBuddyId: string,
  previousRevision: number,
  previousProfileRevision: number | null,
): string | null {
  const catalog = buddyCatalogOf(snapshots.current);
  if (catalog === null) return null;
  if (operation === 'create_buddy' || operation === 'switch_buddy') {
    if (catalog.activeBuddyId === null) {
      return '桌面宿主未签发活动伙伴，未确认本次操作。';
    }
    if (operation === 'create_buddy' && catalog.activeBuddyId === previousBuddyId) {
      return '桌面宿主未切换到新伙伴，未确认本次新建。';
    }
    if (operation === 'switch_buddy' && catalog.activeBuddyId !== targetId) {
      return '桌面宿主未切换到所选伙伴，请重新读取状态后再操作。';
    }
    if (catalog.buddiesRevision <= previousRevision) {
      return '伙伴选择修订未推进，未确认本次操作。';
    }
    return null;
  }
  if (catalog.activeBuddyId !== previousBuddyId) {
    return '改名后当前伙伴发生变化，请重新读取状态。';
  }
  const target = catalog.buddies.find((buddy) => buddy.id === targetId);
  if (target === undefined) {
    return '桌面宿主未返回被改名的伙伴，请重新读取状态。';
  }
  if (previousProfileRevision !== null && target.profileRevision <= previousProfileRevision) {
    return '伙伴档案版本未推进，未确认本次改名。';
  }
  if (catalog.buddiesRevision <= previousRevision) {
    return '伙伴选择修订未推进，未确认本次改名。';
  }
  return null;
}

function buddyName(buddy: BuddyView): string {
  return buddyDisplayLabel(buddy);
}

/** 提交时捕获的表单快照；回包到达后用于判断是否出现新编辑。 */
interface BuddyFormCapture {
  createOpen: boolean;
  label: string;
  persona: string;
  renameOpen: boolean;
  renameLabel: string;
}

function captureBuddyForm(): BuddyFormCapture {
  return {
    createOpen: buddyCreateOpen.value,
    label: buddyForm.label,
    persona: buddyForm.persona,
    renameOpen: buddyRenameOpen.value,
    renameLabel: buddyRenameLabel.value,
  };
}

function buddyFormsDiffer(a: BuddyFormCapture, b: BuddyFormCapture): boolean {
  return (
    a.createOpen !== b.createOpen ||
    a.label !== b.label ||
    a.persona !== b.persona ||
    a.renameOpen !== b.renameOpen ||
    a.renameLabel !== b.renameLabel
  );
}

/**
 * 成功提交后只在宿主已把管理草稿消费为 clean/none 且版本不旧于提交版本时
 * 才撤掉表单保护。宿主仍报告 dirty 或更旧版本时保留表单并如实报告。
 */
function managementDraftConsumed(version: number): boolean {
  const draft = snapshot.value?.managementDraft;
  if (draft === undefined || draft === null) return false;
  if (draft.dirty || draft.kind !== 'none' || draft.version < version) return false;
  return managementDraftSync.adopt(draft, buddyTarget);
}

/** 提交后用户已把表单完全关闭/清空时，没有需要保留的本地新编辑。 */
function buddyFormBlank(intent: BuddyCommitIntent): boolean {
  if (intent.kind === 'create_buddy') {
    return (
      !buddyCreateOpen.value
      && buddyForm.label.length === 0
      && buddyForm.persona.length === 0
    );
  }
  if (intent.kind === 'rename_buddy') {
    return !buddyRenameOpen.value && buddyRenameLabel.value.length === 0;
  }
  return true;
}

/**
 * 消费与本次伙伴动作精确匹配的 `buddyCommit`。事件先到、回复先到和回复丢失后
 * 重读都走这里，避免两个入口对表单做不同处理：
 * - 匹配后本次意图只消费一次，重复事件/重复回复不再清稿；
 * - 提交后的新编辑（正文、管理信号、管理版本任一推进）只确认旧保存，
 *   保留新稿并让 `adoptBuddyScope` 重新登记保护；
 * - 没有新编辑且宿主已按精确版本消费管理草稿时才撤表单，否则保留并如实报告。
 */
function consumeCommittedBuddyAction(current: PrototypeSnapshot | null): void {
  const intent = buddyCommitIntent;
  if (intent === null || intent.applied || current === null || current.revoked) return;
  if (!buddyCommitMatchesIntent(intent, current.buddyCommit)) return;
  intent.applied = true;
  // 该回执证明本次伙伴动作已完成，先按发起伙伴收口对应 Unknown。
  if (unconfirmedOperation === intent.kind) {
    commandLock.confirmScoped(intent.kind, intent.fromBuddyId, null);
    if (!commandLock.locked) unconfirmedOperation = null;
    syncCommandUnconfirmed();
  }
  if (intent.kind === 'switch_buddy') {
    intent.formSettled = true;
    return;
  }
  const formBlank = buddyFormBlank(intent);
  intent.newerEdits = !formBlank && (
    intent.submittedSync.version !== intent.submittedSyncVersion
    || managementDraftSignal.value !== intent.submittedSignal
    || buddyFormsDiffer(captureBuddyForm(), intent.submittedForm)
  );
  if (!intent.newerEdits && managementDraftConsumed(intent.managementVersion)) {
    clearConsumedBuddyForm(intent);
    intent.formSettled = true;
    return;
  }
  // 有新编辑或宿主尚未按精确版本消费：保留正文，交给作用域采纳时重新登记保护。
  intent.preservedForm = true;
  intent.formSettled = false;
}

/** 只清除本次已确认消费的表单，不碰其他字段或另一份表单。 */
function clearConsumedBuddyForm(intent: BuddyCommitIntent): void {
  if (intent.kind === 'create_buddy') {
    if (buddyCreateOpen.value) {
      buddyCreateOpen.value = false;
      buddyForm.label = '';
      buddyForm.persona = '';
    }
  } else if (intent.kind === 'rename_buddy') {
    if (buddyRenameOpen.value) {
      buddyRenameOpen.value = false;
      buddyRenameLabel.value = '';
    }
  }
  lastManagementDraftSignal = managementDraftSignal.value;
}

/**
 * 作用域变化是否来自本次尚未结算的伙伴提交。仅用于避免在确认期间清掉
 * 创建/改名表单；不把表单内容自动提交或复制到另一伙伴。
 */
function pendingBuddyCommitChangesScope(
  previous: PrototypeSnapshot | null,
  current: PrototypeSnapshot | null,
): boolean {
  const intent = buddyCommitIntent;
  if (intent === null || intent.kind === 'switch_buddy' || current === null || current.revoked) {
    return false;
  }
  if (intent.applied) return intent.preservedForm;
  const next = buddyExpectationOf(current);
  if (intent.kind === 'create_buddy') {
    if (next.expectedBuddyId === intent.fromBuddyId) return false;
    // 未收到匹配回执时只保护“刚出现的新伙伴”，不把旧表单带到一个已存在的
    // 其他伙伴；否则无法证明这次作用域变化来自本次创建。
    const catalog = buddyCatalogOf(previous);
    return catalog === null || !catalog.buddies.some((buddy) => buddy.id === next.expectedBuddyId);
  }
  return next.expectedBuddyId === intent.fromBuddyId;
}

/**
 * 作用域切换后，保留的更新稿仍属于用户当前编辑的伙伴：在新作用域重新登记
 * dirty/种类，恢复退出保护；不自动派发保存，也不把正文搬到另一伙伴。
 */
function recordPreservedBuddyCommitForm(): void {
  const intent = buddyCommitIntent;
  if (intent === null || !intent.preservedForm || intent.protectionRecorded) return;
  intent.protectionRecorded = true;
  const desired = managementDraftIntent.value;
  if (desired.dirty) {
    if (!managementDraftSync.record(desired.dirty, desired.kind, buddyTarget)) {
      setError('更新后的伙伴表单无法重新登记退出保护；请重新读取宿主状态后再保存。', null);
    }
  }
  lastManagementDraftSignal = managementDraftSignal.value;
}

/**
 * 创建/切换/改名共用流程：先确认没有未保存管理编辑与在途发送，再同步草稿，
 * 成功后才派发；失败或回包丢失都保留原选择、原草稿与 API Key 输入，不自动重发。
 * 创建/改名提交只消费与发起时同伙伴、同选择修订、同种类且精确版本的表单。
 */
async function runBuddyAction(
  operation: 'create_buddy' | 'switch_buddy' | 'rename_buddy',
  targetId: string | null,
  previousProfileRevision: number | null,
  expectedKind: 'buddy_create' | 'buddy_rename' | null,
  build: (context: BuddyExpectation, managementDraftVersion: number) => DispatchCommand,
  onConsumed: (() => void) | null = null,
): Promise<boolean> {
  if (busy.value) return false;
  const block =
    operation === 'create_buddy'
      ? createBuddyBlockReason.value
      : operation === 'rename_buddy'
        ? renameBuddyBlockReason.value
        : buddyActionBlockReason.value;
  if (block !== null) {
    buddyError.value = block;
    return false;
  }
  // 首次点击即冻结完整伙伴 context（ID + 选择修订）。之后每个同步等待边界
  // 都只与它比较；等待期间推进到 r5 时必须取消本次操作，不能拿 r5 重新派发
  // 一个在 r4 下发起的意图。
  const firstClick = buddyExpectationOf(snapshots.current);
  const firstClickScopeGeneration = buddyScopeGeneration;
  const previousBuddyId = firstClick.expectedBuddyId;
  const actionContextStill = (): boolean =>
    !disposed
    && buddyScopeGeneration === firstClickScopeGeneration
    && buddyExpectationMatches(buddyExpectationOf(snapshots.current), firstClick);
  // 正文由点击入口同步捕获；它的表单快照和保护版本也必须在首次 await 前固定。
  // 等待同步期间出现新编辑时取消旧提交，不能用新版本替旧正文签收。
  const submittedSync = managementDraftSync;
  const submittedScopeGeneration = firstClickScopeGeneration;
  const submittedForm = captureBuddyForm();
  const submittedSignal = managementDraftSignal.value;
  const firstManagementVersion = submittedSync.version;
  const submittedFormStill = (): boolean =>
    managementDraftSync === submittedSync
    && managementDraftSync.version === firstManagementVersion
    && managementDraftSignal.value === submittedSignal
    && !buddyFormsDiffer(captureBuddyForm(), submittedForm);
  if (expectedKind !== null && (!submittedSync.dirty || submittedSync.kind !== expectedKind)) {
    buddyError.value = '伙伴表单尚未同步为待提交状态，本次保存已取消；表单已保留。';
    return false;
  }
  const previousSessionId = activeSessionId.value;
  let previousDraft = draft.value;
  if (previousSessionId !== null) rememberSessionDraft(previousSessionId, previousDraft);

  // 草稿同步成功后才能切换/创建；失败保留原选择和文本。
  if (!activeSessionArchived.value) {
    const synced = await draftSync.flushLatest(draft.value);
    if (disposed) return false;
    if (!synced) {
      setError('草稿尚未同步到桌面宿主，本次伙伴操作已取消；草稿已保留。', null);
      return false;
    }
  }
  if (disposed) return false;
  if (!actionContextStill()) {
    setError('伙伴上下文在草稿同步期间发生变化，本次操作已取消；草稿与表单已保留。', null);
    return false;
  }
  if (expectedKind !== null && !submittedFormStill()) {
    setError('等待同步期间表单有新编辑，本次保存已取消；新稿已保留。', null);
    return false;
  }
  // 管理编辑状态也必须已确认同步；界面不能用未保存编辑绕过退出保护。
  if (!(await managementDraftSync.flush())) {
    if (!disposed) setError('管理编辑状态尚未同步到桌面宿主，本次伙伴操作已取消。', null);
    return false;
  }
  if (disposed) return false;
  if (!actionContextStill()) {
    setError('伙伴上下文在管理编辑同步期间发生变化，本次操作已取消；草稿与表单已保留。', null);
    return false;
  }
  let managementDraftVersion = 0;
  if (expectedKind !== null) {
    if (
      !submittedFormStill() ||
      buddyScopeGeneration !== submittedScopeGeneration ||
      !managementDraftSync.dirty ||
      managementDraftSync.kind !== expectedKind ||
      !managementDraftSync.isSynced
    ) {
      setError('伙伴表单在同步期间发生变化，本次保存已取消；表单已保留。', null);
      return false;
    }
    managementDraftVersion = firstManagementVersion;
  }
  previousDraft = draft.value;
  if (previousSessionId !== null) rememberSessionDraft(previousSessionId, previousDraft, true);
  const capture: BuddyActionCapture = {
    expectedBuddyId: firstClick.expectedBuddyId,
    expectedBuddiesRevision: firstClick.expectedBuddiesRevision,
    generation: firstClickScopeGeneration,
    draftEditGeneration,
  };
  const submittedVersion = expectedKind !== null
    ? firstManagementVersion
    : managementDraftSync.version;
  const commitment: BuddyCommitIntent = {
    kind: operation,
    fromBuddyId: firstClick.expectedBuddyId,
    fromRevision: firstClick.expectedBuddiesRevision,
    managementVersion: submittedVersion,
    submittedForm,
    submittedSignal,
    submittedSync,
    submittedSyncVersion: submittedVersion,
    applied: false,
    newerEdits: false,
    preservedForm: false,
    formSettled: false,
    protectionRecorded: false,
  };
  buddyCommitIntent = commitment;

  busy.value = true;
  buddyError.value = null;
  clearError();
  try {
    // 派发使用首次点击捕获的完整 context，不用等待后重读的“最新值”。
    const response = await dispatchPrototype(
      build(firstClick, managementDraftVersion),
      firstClick,
    );
    if (disposed) return false;
    // 撤权回包必须先进统一清理路径；不能被“注册表较旧”提前丢弃。
    if (response.snapshot?.revoked === true) {
      applyResponse(response, operation, null, previousBuddyId);
      return false;
    }
    // 改名不得改变活动伙伴；创建/切换本来就要换伙伴，由结果语义检查裁决。
    if (operation === 'rename_buddy' && responseBuddyMismatch(response, previousBuddyId)) {
      setError('收到其他伙伴的注册表回包，已忽略本次结果；选择与草稿保持原样。', null);
      return false;
    }
    const update = applyResponse(response, operation, null, previousBuddyId);
    // 事件先到与回复先到共用同一消费结果；matched 表示本次匹配回执已被消费。
    const matched = commitment.applied;
    if (!matched && update.kind === 'failure' && !update.stale) {
      // 明确未执行：这次动作不可能再产生匹配回执，不保留旧意图。
      if (buddyCommitIntent === commitment) buddyCommitIntent = null;
      // 拒绝/失败不清稿：只在伙伴、作用域代际与编辑代际都未变时恢复原草稿。
      if (currentBuddyStill(capture)) {
        if (previousSessionId !== null) {
          rememberSessionDraft(previousSessionId, previousDraft, true);
        }
        if (!disposed) draft.value = previousDraft;
      }
      return false;
    }
    if (!matched && update.kind !== 'success') {
      if (!disposed && expectedKind !== null) {
        setError(
          '未从事件或回复读取到本次保存的匹配成功回执；表单已保留，未确认本次保存。',
          snapshots.current?.revision ?? null,
        );
      }
      return false;
    }
    if (!matched && expectedKind !== null) {
      // 成功但缺回执：作用域可能已切换，仍须把保留下来的表单重新登记到
      // 当前作用域，不能因为缺少收据而丢失退出保护。
      commitment.preservedForm = true;
      recordPreservedBuddyCommitForm();
    }
    const problem = buddyActionOutcomeProblem(
      operation,
      targetId,
      previousBuddyId,
      capture.expectedBuddiesRevision,
      previousProfileRevision,
    );
    if (problem !== null) {
      setError(problem, snapshots.current?.revision ?? null);
      return false;
    }
    if (expectedKind !== null) {
      // 提交后用户继续编辑，或宿主尚未按精确版本消费：不得静默清掉当前表单。
      if (!matched || !commitment.formSettled) {
        setError(
          commitment.newerEdits
            ? '保存回包到达前出现了新的表单编辑；本次只确认已提交版本，新编辑仍受退出保护。'
            : matched
              ? '保存结果已读取，但宿主尚未确认消费本次表单草稿；表单已保留以防未保存正文丢失。'
              : '保存结果未包含匹配的成功回执；表单已保留，未确认本次保存。',
          snapshots.current?.revision ?? null,
        );
        return false;
      }
      onConsumed?.();
      lastManagementDraftSignal = managementDraftSignal.value;
    }
    buddyNotice.value =
      operation === 'create_buddy'
        ? '已创建并选择新伙伴；新伙伴不继承任何旧伙伴的私密经历或凭据。'
        : operation === 'rename_buddy'
          ? '伙伴名称已更新；实例身份、人设与模型绑定未改变。'
          : '已切换伙伴；各伙伴的会话、草稿与配置互不串档。';
    archiveArmed.value = false;
    return true;
  } catch (error) {
    if (!disposed) {
      if (commitment.applied) {
        // 事件已证明本次动作完成；回复丢失不再是 Unknown。更新稿仍保留。
        if (expectedKind !== null && !commitment.formSettled) {
          setError(
            commitment.newerEdits
              ? '保存已确认，但回包前出现了新的表单编辑；新编辑仍受退出保护。'
              : '保存已确认，但回包丢失且宿主尚未确认消费表单草稿；表单已保留。',
            snapshots.current?.revision ?? null,
          );
          return false;
        }
        buddyNotice.value =
          operation === 'create_buddy'
            ? '已创建并选择新伙伴；新伙伴不继承任何旧伙伴的私密经历或凭据。'
            : operation === 'rename_buddy'
              ? '伙伴名称已更新；实例身份、人设与模型绑定未改变。'
              : '已切换伙伴；各伙伴的会话、草稿与配置互不串档。';
        archiveArmed.value = false;
        return true;
      }
      // 注册表回包丢失时无法确定选择是否已变化，按全局未知保守锁定；不自动重发。
      // 保留意图供迟到事件/匹配回执收口，但绝不拿最新 context 自行重发。
      markCommandUnknown(operation, 'ipc_reply_lost', null, previousBuddyId);
      setError(describeThrownFailure(operation, error), null);
      if (currentBuddyStill(capture)) {
        if (previousSessionId !== null) {
          rememberSessionDraft(previousSessionId, previousDraft, true);
        }
        draft.value = previousDraft;
      }
    }
    return false;
  } finally {
    if (!disposed) {
      busy.value = false;
      void nextTick(() => inputRef.value?.focus());
    }
    if (
      buddyCommitIntent === commitment
      && commitment.applied
      && commitment.formSettled
    ) {
      buddyCommitIntent = null;
    }
  }
}

function openBuddyCreate(): void {
  if (buddyCreateOpen.value) return;
  if (createBuddyBlockReason.value !== null) return;
  // 打开另一份空表单不丢稿；有未保存编辑时上面的阻塞原因已拒绝。
  buddyRenameOpen.value = false;
  buddyRenameLabel.value = '';
  buddyCreateOpen.value = true;
  buddyForm.label = '';
  buddyForm.persona = '';
  buddyError.value = null;
  buddyNotice.value = null;
  clearError();
}

function cancelBuddyCreate(): void {
  buddyCommitIntent = null;
  buddyCreateOpen.value = false;
  buddyForm.label = '';
  buddyForm.persona = '';
  buddyError.value = null;
}

function submitBuddyCreate(): void {
  if (busy.value) {
    buddyError.value = '当前有未完成的操作，暂不能创建伙伴。';
    return;
  }
  const label = buddyForm.label;
  const labelProblem = buddyLabelProblem(label);
  if (labelProblem !== null) {
    buddyError.value = labelProblem;
    return;
  }
  const persona = buddyForm.persona;
  const personaProblem = buddyPersonaProblem(persona);
  if (personaProblem !== null) {
    buddyError.value = personaProblem;
    return;
  }
  if (createBuddyBlockReason.value !== null) {
    buddyError.value = createBuddyBlockReason.value;
    return;
  }
  void runBuddyAction(
    'create_buddy',
    null,
    null,
    'buddy_create',
    (_context, managementDraftVersion) =>
      createBuddyCommand(label, persona, managementDraftVersion),
    () => {
      buddyCreateOpen.value = false;
      buddyForm.label = '';
      buddyForm.persona = '';
    },
  );
}

function openBuddyRename(): void {
  const current = activeBuddy.value;
  if (buddyRenameOpen.value) return;
  if (current === null || renameBuddyBlockReason.value !== null) return;
  // 打开另一份空表单不丢稿；有未保存编辑时上面的阻塞原因已拒绝。
  buddyCreateOpen.value = false;
  buddyForm.label = '';
  buddyForm.persona = '';
  buddyRenameOpen.value = true;
  buddyRenameLabel.value = current.label;
  buddyError.value = null;
  buddyNotice.value = null;
  clearError();
}

function cancelBuddyRename(): void {
  buddyCommitIntent = null;
  buddyRenameOpen.value = false;
  buddyRenameLabel.value = '';
  buddyError.value = null;
}

function submitBuddyRename(): void {
  const current = activeBuddy.value;
  if (current === null || busy.value) return;
  const label = buddyRenameLabel.value;
  const labelProblem = buddyLabelProblem(label);
  if (labelProblem !== null) {
    buddyError.value = labelProblem;
    return;
  }
  if (renameBuddyBlockReason.value !== null) {
    buddyError.value = renameBuddyBlockReason.value;
    return;
  }
  if (label.trim() === current.label.trim()) {
    // 名称未变化：不为纯展示编辑推进档案版本。
    cancelBuddyRename();
    return;
  }
  const targetId = current.id;
  const profileRevision = current.profileRevision;
  void runBuddyAction(
    'rename_buddy',
    targetId,
    profileRevision,
    'buddy_rename',
    (_context, managementDraftVersion) =>
      renameBuddyCommand(label, managementDraftVersion),
    () => {
      buddyRenameOpen.value = false;
      buddyRenameLabel.value = '';
    },
  );
}

async function switchToBuddy(id: string): Promise<void> {
  if (id === activeBuddyId.value) return;
  if (!isSafeBuddyId(id)) {
    buddyError.value = '该伙伴不在宿主签发的注册表中，请重新读取状态。';
    return;
  }
  const target = buddies.value.find((buddy) => buddy.id === id);
  if (target === undefined) {
    buddyError.value = '该伙伴不在宿主签发的注册表中，请重新读取状态。';
    return;
  }
  // 切换前不预先关闭表单；未保存编辑由共同前置拒绝并原样保留。
  await runBuddyAction(
    'switch_buddy',
    id,
    null,
    null,
    () => switchBuddyCommand(id),
  );
}

async function togglePetPause(): Promise<void> {
  try {
    await invokeWindowCommand('toggle_pause');
    await refreshPausedFromHost();
  } catch (error) {
    if (!disposed) setError(describeThrownFailure('window', error), null);
  }
}

/** “显示宠物”必须真正恢复 pet 窗口，不借隐藏聊天冒充。 */
async function showPet(): Promise<void> {
  try {
    await invokeWindowCommand('show_pet');
  } catch (error) {
    if (!disposed) setError(describeThrownFailure('window', error), null);
  }
}

/** 隐藏聊天是独立且语义明确的窗口操作，与显示宠物互不替代。 */
async function hideChat(): Promise<void> {
  // 发起时即捕获完整伙伴上下文；宿主排队期间的选择变化不得改写本次命令。
  const capturedBuddy = buddyExpectationOf(snapshots.current);
  try {
    await invokeWindowCommand('hide_chat', capturedBuddy);
  } catch (error) {
    if (!disposed) setError(describeThrownFailure('window', error), null);
  }
}

function roleLabel(role: string): string {
  if (role === 'user') return '你';
  if (role === 'assistant') return '伙伴';
  if (role === 'system') return '系统';
  return role.length > 0 ? role : '消息';
}

function messageClass(role: string): string {
  if (role === 'user') return 'from-user';
  if (role === 'system') return 'from-system';
  return 'from-assistant';
}

function scrollToBottom(): void {
  void nextTick(() => {
    const element = messagesEl.value;
    if (element !== null) element.scrollTop = element.scrollHeight;
  });
}

async function detachListeners(): Promise<PrototypeError | null> {
  const attached = unlisteners;
  unlisteners = [];
  const outcome = await releaseListeners(attached);
  if (outcome.ok) {
    delete document.documentElement.dataset.listenerRelease;
    return null;
  }
  // 释放失败时保留句柄，下次重连仍先尝试释放，不叠加新监听。
  unlisteners = attached;
  document.documentElement.dataset.listenerRelease = 'failed';
  return outcome.error;
}

async function initialize(): Promise<void> {
  if (initializing || disposed) return;
  initializing = true;
  // 刷新未知状态期间保持现有锁；只有下面的屏障读取成功才解除。
  commandLock.beginRefresh();
  try {
    // 重连前必须先确认旧监听已释放；失败时不能伪装成已释放并叠加新监听。
    const releaseError = await detachListeners();
    if (disposed) return;
    if (releaseError !== null) {
      commandLock.finishRefresh(false, false);
      syncCommandUnconfirmed();
      connectionState.value = 'failed';
      fatalMessage.value = formatSafeError(releaseError);
      return;
    }

    snapshots.reset();
    snapshot.value = null;
    connectionState.value = 'loading';
    fatalMessage.value = '';
    clearError();
    revokeArmed.value = false;
    archiveArmed.value = false;
    paused.value = null;
    frameEventCount = 0;
    receivedSuccessfulSnapshot = false;
    draft.value = '';
    composing.value = false;
    draftTarget = sessionExpectation(null);
    // 重连先回到“未知伙伴作用域”，由首个权威注册表快照重新采纳；旧伙伴的
    // 未知锁仍按各自作用域保留，普通重读不能解除它们。
    buddyTarget = buddyExpectationOf(null);
    buddyScopeGeneration += 1;
    managementDraftSync.cancel();
    managementDraftSync = createManagementDraftSync();
    managementSyncRevision.value += 1;
    buddyCreateOpen.value = false;
    buddyRenameOpen.value = false;
    buddyForm.label = '';
    buddyForm.persona = '';
    buddyRenameLabel.value = '';
    buddyNotice.value = null;
    buddyError.value = null;
    closeDialogBuddyId.value = null;
    draftSync.cancel();
    draftSync = createDraftSync();
    lastManagementDraftSignal = managementDraftSignal.value;

    let unlistenSnapshot: Unlisten;
    try {
      unlistenSnapshot = await listenSnapshotEvents(handleSnapshotEvent);
    } catch {
      throw new PrototypeBoundaryError(
        'snapshot_listen_failed',
        '无法监听桌面聊天状态',
      );
    }
    if (disposed) {
      await releaseListeners([unlistenSnapshot]);
      return;
    }
    unlisteners.push(unlistenSnapshot);

    try {
      const unlistenFrame = await listenFrameEvents(handleFrameEvent);
      if (disposed) {
        await releaseListeners([unlistenFrame]);
        return;
      }
      unlisteners.push(unlistenFrame);
    } catch {
      // 主窗的暂停徽标是可选的窗口状态；暂停命令仍可调用，不影响聊天主链。
      paused.value = null;
    }

    const response = await requestSnapshot();
    if (disposed) return;
    applyResponse(response, 'load');
    if (response.error !== null) {
      commandLock.finishRefresh(false, false);
      syncCommandUnconfirmed();
      connectionState.value = 'failed';
      fatalMessage.value = describeHostFailure('load', response.error);
      return;
    }
    const current = snapshots.current;
    if (current === null) {
      commandLock.finishRefresh(false, false);
      syncCommandUnconfirmed();
      connectionState.value = 'failed';
      fatalMessage.value = '桌面宿主未返回初始聊天状态。';
      return;
    }
    // 屏障读取还必须不旧于当前已应用的状态，避免旧缓存响应解除未知命令锁。
    const readIsCurrent =
      response.snapshot !== null && response.snapshot.revision >= current.revision;
    commandLock.finishRefresh(readIsCurrent, true);
    syncCommandUnconfirmed();
    connectionState.value = 'ready';
    fatalMessage.value = '';
    void refreshPausedFromHost();
  } catch (error) {
    if (disposed) return;
    commandLock.finishRefresh(false, false);
    syncCommandUnconfirmed();
    const message = describeThrownFailure('load', error);
    if (receivedSuccessfulSnapshot) {
      connectionState.value = 'ready';
      fatalMessage.value = '';
      setError(message, null);
    } else {
      connectionState.value = 'failed';
      fatalMessage.value = message;
    }
  } finally {
    initializing = false;
  }
}

watch(
  () => messages.value.length,
  () => scrollToBottom(),
);

watch(
  () => messages.value[messages.value.length - 1]?.text,
  () => scrollToBottom(),
);

watch(
  managementDraftSignal,
  (signal) => {
    if (snapshot.value?.revoked === true) return;
    // 回包基线、作用域清场与成功消费都不算新的本编辑。
    if (signal === lastManagementDraftSignal) return;
    lastManagementDraftSignal = signal;
    const intent = managementDraftIntent.value;
    if (!managementDraftSync.record(intent.dirty, intent.kind, buddyTarget)) {
      setError('管理编辑版本已达到上限，退出保护状态无法继续同步。', null);
    }
  },
);

// 归档确认只针对确认时的当前会话；目录选择变化后必须重新确认。
watch(activeSessionId, () => {
  archiveArmed.value = false;
});

onMounted(() => {
  void initialize();
});

onBeforeUnmount(() => {
  disposed = true;
  modelCommitIntent = null;
  buddyCommitIntent = null;
  modelInteractionGeneration += 1;
  managementInteractionGeneration += 1;
  buddyScopeGeneration += 1;
  draftSync.cancel();
  managementDraftSync.cancel();
  buddyDrafts.clear();
  buddyDiscardWatermarks.clear();
  archiveArmed.value = false;
  closeDialogBuddyId.value = null;
  panelGuard.cancel();
  discardPanelDrafts();
  snapshots.reset();
  snapshot.value = null;
  const attached = unlisteners;
  unlisteners = [];
  void releaseListeners(attached).then((outcome) => {
    if (!outcome.ok) {
      document.documentElement.dataset.listenerRelease = 'failed';
    }
  });
});
</script>


<template>
  <div class="main-view">
    <!-- 本地装饰 SVG：仅作界面插画与图标，不代表当前素材或任何业务状态。 -->
    <svg class="svg-defs" aria-hidden="true" focusable="false">
      <defs>
        <symbol id="mb-icon-chat" viewBox="0 0 24 24"><path d="M20 11.4a8.4 8.4 0 0 1-8.5 8.2 9 9 0 0 1-3.4-.7L3 20l1.2-4.5A8 8 0 0 1 3 11.4a8.5 8.5 0 0 1 17 0Z"/><path d="M8 10h8m-8 4h5"/></symbol>
        <symbol id="mb-icon-buddy" viewBox="0 0 24 24"><path d="M5 10 4 3l6 4h4l6-4-1 7a7 7 0 0 1 1 4c0 4-3.6 7-8 7s-8-3-8-7a7 7 0 0 1 1-4Z"/><path d="M8 13h.01M16 13h.01M10 17q2 2 4 0"/></symbol>
        <symbol id="mb-icon-sparkle" viewBox="0 0 24 24"><path d="m12 3 2.4 6.6L21 12l-6.6 2.4L12 21l-2.4-6.6L3 12l6.6-2.4Z"/></symbol>
        <symbol id="mb-icon-memory" viewBox="0 0 24 24"><path d="M5 4h6v16H5a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2Zm6 0h8a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-8m4-12h3m-3 4h3m-3 4h2"/></symbol>
        <symbol id="mb-icon-sliders" viewBox="0 0 24 24"><path d="M4 7h3m4 0h9M4 17h9m4 0h3"/><circle cx="9" cy="7" r="2"/><circle cx="15" cy="17" r="2"/></symbol>
        <symbol id="mb-icon-pulse" viewBox="0 0 24 24"><path d="M3 12h4l3-7 4 14 3-7h4"/></symbol>
        <symbol id="mb-icon-plus" viewBox="0 0 24 24"><path d="M12 5v14M5 12h14"/></symbol>
        <symbol id="mb-icon-send" viewBox="0 0 24 24"><path d="m21 3-6 18-4-8-8-4Zm0 0L11 13"/></symbol>
        <symbol id="mb-icon-search" viewBox="0 0 24 24"><circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 5 5"/></symbol>
        <symbol id="mb-icon-refresh" viewBox="0 0 24 24"><path d="M20 7v5h-5M4 17v-5h5m10.4-3A8 8 0 0 0 5 6m-.4 9A8 8 0 0 0 19 18"/></symbol>
        <symbol id="mb-icon-lock" viewBox="0 0 24 24"><rect x="5" y="10" width="14" height="11" rx="3"/><path d="M8 10V7a4 4 0 0 1 8 0v3m-4 4v3"/></symbol>
        <symbol id="mb-icon-info" viewBox="0 0 24 24"><circle cx="12" cy="12" r="9"/><path d="M12 11v6m0-10h.01"/></symbol>
        <symbol id="mb-icon-window" viewBox="0 0 24 24"><rect x="3" y="4" width="18" height="16" rx="3"/><path d="M3 9h18m-14 5h4"/></symbol>
        <symbol id="mb-icon-exit" viewBox="0 0 24 24"><path d="M10 4H5v16h5m-1-8h12m-4-4 4 4-4 4"/></symbol>
        <symbol id="mb-cat" viewBox="0 0 180 180">
          <path d="M42 124C21 126 14 112 22 101" fill="none" stroke="#879d72" stroke-width="12" stroke-linecap="round"/>
          <path d="M48 96C37 119 43 146 61 149h58c18-4 24-27 12-53" fill="#e3eaca" stroke="#82976b" stroke-width="2.5"/>
          <path d="m43 67 1-38c0-4 4-5 7-2l25 22c9-2 20-2 29 0l26-22c3-3 7-2 7 2l-1 38c21 36 1 66-47 66S22 104 43 67Z" fill="#f3f4dc" stroke="#82976b" stroke-width="2.5"/>
          <path d="m49 38 1 25 16-12m64-13-1 25-16-12" fill="#dbb5a1" opacity=".7"/>
          <path d="M64 87q7-8 14 0m25 0q7-8 14 0" stroke="#63754f" stroke-width="3" fill="none" stroke-linecap="round"/>
          <path d="m86 95 5 5 5-5" fill="#9c9570"/><path d="M91 100v4m-9-1q4 6 9 1 5 5 9-1" stroke="#78845d" stroke-width="2" fill="none" stroke-linecap="round"/>
          <ellipse cx="58" cy="98" rx="9" ry="5" fill="#e9c0a6" opacity=".65"/><ellipse cx="123" cy="98" rx="9" ry="5" fill="#e9c0a6" opacity=".65"/>
          <path d="M68 139v10m43-10v10" stroke="#82976b" stroke-width="2.5" stroke-linecap="round"/>
          <path d="m83 54 5 9m11-9-4 9" stroke="#c4cfaa" stroke-width="3" stroke-linecap="round"/>
        </symbol>
      </defs>
    </svg>

    <!-- 常驻外壳：侧栏与页头从挂载起就存在；连接状态只占据工作区，
         让导航契约与键盘路径在“正在连接”期间同样可预测。 -->
    <div class="app-shell">
      <aside class="sidebar" aria-label="主导航">
        <div class="brand">
          <span class="brand-mark" aria-hidden="true"><svg class="icon"><use href="#mb-icon-buddy" /></svg></span>
          <div class="brand-name">
            <strong>Moe Buddy</strong>
            <small>ALWAYS AROUND</small>
          </div>
        </div>
        <template v-for="group in NAV_GROUPS" :key="group.label">
          <p class="nav-label">{{ group.label }}</p>
          <nav class="navigation" aria-label="页面">
            <button
              v-for="page in group.pages"
              :key="page"
              type="button"
              class="nav-item"
              :data-page="page"
              :aria-current="activePage === page ? 'page' : undefined"
              :disabled="pageNavBlockReason(page) !== null"
              :title="pageNavBlockReason(page) ?? PAGES[page].name"
              @click="requestPage(page)"
            >
              <svg class="icon" aria-hidden="true"><use :href="`#mb-icon-${PAGE_ICONS[page]}`" /></svg>
              <span class="nav-text">{{ PAGES[page].nav }}</span>
              <span v-if="activePage === page" class="nav-arrow" aria-hidden="true">›</span>
            </button>
          </nav>
        </template>
        <div class="sidebar-bottom">
          <div v-if="!revoked" class="companion-mini">
            <span class="avatar" aria-hidden="true"><svg><use href="#mb-cat" /></svg></span>
            <div class="grow">
              <strong>{{ activeBuddy ? buddyName(activeBuddy) : '未签发伙伴' }}</strong>
              <small>当前伙伴</small>
            </div>
            <span class="dot" aria-hidden="true"></span>
          </div>
          <button
            v-if="snapshot !== null && !revoked"
            type="button"
            class="sidebar-revoke"
            data-testid="revoke-authorization"
            aria-label="撤回授权"
            title="撤回本次授权；确认后清空当前展示与草稿"
            :disabled="!canRequestRevoke"
            @click="requestRevoke"
          >
            <svg class="icon" aria-hidden="true"><use href="#mb-icon-lock" /></svg>
            <span>撤回授权</span>
          </button>
          <div class="sidebar-tools">
            <button
              type="button"
              :disabled="busy || connectionState !== 'ready'"
              title="隐藏主窗；宿主确认后才改变窗口可见性"
              @click="hideChat"
            >
              <svg class="icon" aria-hidden="true"><use href="#mb-icon-window" /></svg>
              <span>隐藏聊天</span>
            </button>
            <button
              type="button"
              data-testid="quit-application"
              :disabled="snapshot === null || interactionLocked"
              title="退出主窗、宠物窗和后台；未保存内容会先提示"
              @click="requestQuit"
            >
              <svg class="icon" aria-hidden="true"><use href="#mb-icon-exit" /></svg>
              <span>退出应用</span>
            </button>
          </div>
        </div>
      </aside>

      <main class="workspace">
        <header class="topbar">
          <div class="breadcrumb">
            <span class="crumb-root">我的陪伴空间</span>
            <span class="slash" aria-hidden="true">/</span>
            <span>{{ PAGES[activePage].name }}</span>
          </div>
          <div class="chat-meta">
            <span v-if="connectionState === 'ready'" class="status-pill" :class="statusTone">状态：{{ statusLabel }}</span>
            <span v-if="revoked" class="revoked-pill">授权已撤回</span>
            <span v-if="paused !== null" class="pet-pill">
              {{ paused ? '宠物已暂停' : '宠物播放中' }}
            </span>
            <span class="pet-pill">
              {{ externalTransmissionAllowed ? '已允许外传' : '仅本地 / 未允许外传' }}
            </span>
            <span v-if="busy || panelBusy" class="pending-pill">等待桌面宿主确认…</span>
            <span class="offline-note">请求经桌面宿主受控发出</span>
          </div>
        </header>

        <div v-if="connectionState === 'loading'" class="center-state">
          <p>正在连接桌面宿主…</p>
          <p class="muted">初始化聊天状态与事件监听。</p>
        </div>

        <div v-else-if="connectionState === 'failed'" class="center-state">
          <p class="center-title">无法连接桌面宿主</p>
          <p class="muted">{{ fatalMessage }}</p>
          <button type="button" class="btn primary" @click="initialize">重试连接</button>
        </div>

        <template v-else>
        <!-- 以下横幅与确认属于整个应用：任何页面都可见，不会被切页隐藏。 -->
        <p v-if="hostNotice" class="recovery-banner" role="status">
          <strong>宿主提示：</strong>{{ hostNotice }}
        </p>
        <p v-if="persistenceNotice" class="chat-warning" role="status">{{ persistenceNotice }}</p>
        <div v-if="lastError" class="error-banner" role="alert">
          <span>{{ lastError }}</span>
          <button type="button" class="ghost" @click="clearError">关闭提示</button>
        </div>
        <div v-if="commandUnconfirmed" class="error-banner" role="status">
          <span>桌面宿主未确认最近一次操作的结果，发送、模型配置与记忆编辑已锁定；请重新读取状态后再操作。</span>
          <button type="button" class="ghost" @click="initialize">重新读取状态</button>
        </div>

        <section
          class="page page-chat"
          data-page-view="chat"
          aria-labelledby="page-chat-title"
          :hidden="activePage !== 'chat'"
        >
          <header class="page-heading chat-heading">
            <div>
              <h1 id="page-chat-title">聊聊今天</h1>
              <p>平凡的小事，也值得被认真听见。</p>
            </div>
            <div class="heading-actions">
              <button
                type="button"
                class="btn quiet"
                :disabled="busy || revoked || closeConfirmation"
                @click="requestClose"
              >
                关闭聊天
              </button>
              <button type="button" class="btn quiet" @click="requestPage('buddies')">
                <svg class="icon" aria-hidden="true"><use href="#mb-icon-buddy" /></svg>
                <span>{{ activeBuddy ? buddyName(activeBuddy) : '选择伙伴' }}</span>
                <span class="muted" aria-hidden="true">⌄</span>
              </button>
            </div>
          </header>
          <div v-if="revoked" class="page-content">
            <div class="empty-state revoked-state">
              <p class="empty-title">授权已撤回</p>
              <p>聊天记录、草稿、模型配置与记忆列表已从界面清空。</p>
              <p>此页面不提供重新授权入口。</p>
            </div>
          </div>
          <div v-else class="chat-body">
            <aside
              class="sessions"
              aria-label="会话列表"
              :title="sessionDirectoryRevision !== null ? `目录修订 ${sessionDirectoryRevision}` : undefined"
            >
              <div class="session-top">
                <strong>我们的对话</strong>
                <button
                  type="button"
                  class="icon-btn"
                  aria-label="新建会话"
                  :disabled="newSessionBlockReason !== null"
                  :title="newSessionBlockReason ?? '新建会话'"
                  @click="startNewSession"
                >
                  <svg class="icon" aria-hidden="true"><use href="#mb-icon-plus" /></svg>
                </button>
              </div>
              <div class="session-scroll">
                <span v-if="!sessionCatalogAvailable" class="session-empty">
                  桌面宿主尚未提供会话目录；新建、切换与归档暂不可用。
                </span>
                <template v-else>
                  <span v-if="sessions.length === 0" class="session-empty">宿主尚未签发任何会话。</span>
                  <button
                    v-for="item in sessions"
                    :key="item.id"
                    type="button"
                    class="session-chip"
                    :class="{ active: item.id === activeSessionId, archived: item.archived }"
                    :disabled="switchSessionBlockReason !== null || item.id === activeSessionId || busy"
                    :title="
                      switchSessionBlockReason ??
                      (item.archived ? '已归档：可只读查看，不能发送' : undefined)
                    "
                    @click="switchToSession(item.id)"
                  >
                    <svg class="icon" aria-hidden="true"><use href="#mb-icon-chat" /></svg>
                    <span class="session-text">
                      <span class="session-name">{{ sessionLabel(item) }}</span>
                      <span class="session-sub">
                        <span v-if="item.archived" class="session-flag">归档</span>
                        <span class="session-status" :class="sessionTone(item)">{{ statusText(item.status) }}</span>
                      </span>
                    </span>
                  </button>
                </template>
              </div>
              <p class="session-bottom">每一段对话各自保存；归档只读，不会删除原文。</p>
            </aside>

            <div class="conversation">
              <div class="conversation-head">
                <div class="row">
                  <span class="avatar" aria-hidden="true"><svg><use href="#mb-cat" /></svg></span>
                  <div class="grow">
                    <strong>{{ activeBuddy ? buddyName(activeBuddy) : '未签发伙伴' }}</strong>
                    <small>
                      <span class="dot" aria-hidden="true"></span>
                      {{ statusLabel }}
                    </small>
                  </div>
                </div>
                <button
                  v-if="activeSession !== null && !activeSession.archived"
                  type="button"
                  class="btn quiet"
                  :disabled="archiveBlockReason !== null"
                  :title="archiveBlockReason ?? undefined"
                  @click="requestArchiveSession"
                >
                  归档会话
                </button>
              </div>

              <div ref="messagesEl" class="messages" aria-live="polite">
                <div v-if="messages.length === 0" class="empty-state">
                  <p class="empty-title">还没有消息</p>
                  <p>这里只展示桌面宿主返回的内容，前端不会模拟回复。</p>
                  <p>Enter 发送，Shift+Enter 换行。</p>
                </div>
                <article
                  v-for="(message, index) in messages"
                  :key="index"
                  class="message"
                  :class="messageClass(message.role)"
                >
                  <span
                    v-if="message.role !== 'user'"
                    class="avatar avatar-sm"
                    aria-hidden="true"
                  ><svg><use href="#mb-cat" /></svg></span>
                  <div class="message-body">
                    <p class="message-meta">{{ roleLabel(message.role) }}</p>
                    <div class="bubble"><p class="text">{{ message.text }}</p></div>
                  </div>
                </article>
              </div>

              <p v-if="activeSessionArchived" class="archived-banner" role="status">
                当前会话已归档：只能只读查看，不能发送或继续编辑草稿。本阶段不提供解归档、删除会话或自动创建下一会话。
              </p>
              <p v-if="sendBlockade" class="chat-warning" role="status">{{ sendBlockade }}</p>
              <p v-if="chatFailure" class="chat-failure" role="alert">{{ chatFailure }}</p>
              <p v-if="sendIsolationAllowed" class="chat-warning" role="status">
                当前会话的上一次发送结果未知：可显式新建隔离会话；旧会话保持 Unknown、草稿按会话保留，不会自动重发。
              </p>
              <p
                v-if="newSessionBlockReason !== null && sessionCatalogAvailable && commandUnconfirmed"
                class="chat-warning"
                role="status"
              >
                {{ newSessionBlockReason }}
              </p>

              <div class="composer-wrap">
                <form class="composer" @submit.prevent="send">
                  <label class="sr-only" for="chat-input">聊天草稿</label>
                  <textarea
                    id="chat-input"
                    ref="inputRef"
                    v-model="draft"
                    :disabled="!canCompose"
                    rows="2"
                    :placeholder="
                      revoked
                        ? '授权已撤回'
                        : activeSessionArchived
                          ? '会话已归档，只读查看'
                          : '想说什么都可以，慢慢来…'
                    "
                    @input="onDraftInput"
                    @keydown="handleKeydown"
                    @compositionstart="onCompositionStart"
                    @compositionend="onCompositionEnd"
                  />
                  <div class="composer-bottom">
                    <small>Enter 发送 · Shift+Enter 换行</small>
                    <button
                      class="btn primary"
                      type="submit"
                      :disabled="!canSend"
                      :title="sendBlockade ?? undefined"
                    >
                      发送
                      <svg class="icon" aria-hidden="true"><use href="#mb-icon-send" /></svg>
                    </button>
                  </div>
                </form>
                <p class="composer-caption">
                  {{ externalTransmissionAllowed
                    ? '已允许向当前端点发送本次会话内容。'
                    : '仅本地 · 连接模型后，需由你允许发送。' }}
                  <button type="button" class="inline-link" @click="requestPage('model')">
                    前往设置
                  </button>
                </p>
              </div>
            </div>
          </div>
        </section>

        <section
          class="page"
          data-page-view="buddies"
          aria-labelledby="page-buddies-title"
          :hidden="activePage !== 'buddies'"
        >
          <header class="page-heading">
            <div>
              <h1 id="page-buddies-title">陪伴，各有模样</h1>
              <p>在这里认识、选择和照顾你的伙伴。</p>
            </div>
            <div class="buddy-actions heading-actions">
              <button
                type="button"
                class="btn primary"
                :disabled="createBuddyBlockReason !== null || buddyCreateOpen"
                :title="createBuddyBlockReason ?? undefined"
                @click="openBuddyCreate"
              >
                <svg class="icon" aria-hidden="true"><use href="#mb-icon-plus" /></svg>
                新建伙伴
              </button>
              <button
                v-if="activeBuddy !== null"
                type="button"
                class="btn"
                :disabled="buddyActionBlockReason !== null || buddyRenameOpen"
                :title="buddyActionBlockReason ?? undefined"
                @click="openBuddyRename"
              >
                伙伴改名
              </button>
            </div>
          </header>
          <div class="page-content">
            <span v-if="!buddyCatalogAvailable" class="session-empty">
              桌面宿主尚未提供伙伴注册表；创建、切换与改名暂不可用。
            </span>
            <template v-else>
              <div class="buddy-hero" aria-hidden="true">
                <div>
                  <span class="micro muted">YOUR LITTLE COMPANION</span>
                  <h2>总有一个小小的身影，<br />愿意听你说完。</h2>
                  <p>每位伙伴都有自己的性格、对话和记忆。</p>
                </div>
                <svg class="hero-cat"><use href="#mb-cat" /></svg>
              </div>
              <div class="section-label">我的伙伴 <span>{{ buddies.length }}</span></div>
              <div v-if="buddies.length === 0" class="empty-state">
                <p class="empty-title">还没有伙伴</p>
                <p>桌面宿主尚未签发任何伙伴；用右上角「新建伙伴」开始第一段陪伴。</p>
              </div>
              <div class="buddy-grid">
                <article
                  v-for="buddy in buddies"
                  :key="buddy.id"
                  class="card buddy-card buddy-chip"
                  :class="{ active: buddy.id === activeBuddyId, selected: buddy.id === activeBuddyId }"
                >
                  <div class="row">
                    <span class="avatar" aria-hidden="true">{{ buddyName(buddy).slice(0, 1) }}</span>
                    <div class="grow">
                      <h3 class="buddy-name">{{ buddyName(buddy) }}</h3>
                      <small class="muted">独立记忆与会话</small>
                    </div>
                    <span v-if="buddy.id === activeBuddyId" class="pill">当前伙伴</span>
                  </div>
                  <details class="buddy-detail">
                    <summary>实例详情</summary>
                    <dl class="buddy-detail-list">
                      <div class="buddy-detail-line">
                        <dt>实例 ID</dt>
                        <dd>{{ buddy.id }}</dd>
                      </div>
                      <div class="buddy-detail-line">
                        <dt>档案 / 人设 / 模型</dt>
                        <dd class="buddy-revisions">{{ buddyRevisionText(buddy) }}</dd>
                      </div>
                    </dl>
                    <p class="muted buddy-detail-note">
                      实例身份由受信服务签发，仅供确认当前资料；界面操作不依赖这些编号。
                    </p>
                  </details>
                  <div class="buddy-card-foot">
                    <div class="row">
                      <button
                        type="button"
                        class="btn"
                        :disabled="buddyActionBlockReason !== null || buddy.id === activeBuddyId || busy"
                        :title="buddyActionBlockReason ?? undefined"
                        @click="switchToBuddy(buddy.id)"
                      >
                        {{ buddy.id === activeBuddyId ? '当前伙伴' : '选择伙伴' }}
                      </button>
                      <button
                        v-if="buddy.id === activeBuddyId"
                        type="button"
                        class="btn quiet"
                        @click="requestPage('chat')"
                      >
                        去聊天
                      </button>
                    </div>
                  </div>
                </article>
              </div>
              <p class="note buddy-identity" role="status">
                <svg class="icon" aria-hidden="true"><use href="#mb-icon-lock" /></svg>
                <span>
                  当前伙伴：{{ activeBuddy ? buddyName(activeBuddy) : '未签发' }}。
                  实例身份由受信服务签发；API Key 与发送许可只属于当前伙伴本次进程，不随伙伴复制或恢复。新伙伴从自己的故事开始，不会继承其他伙伴的私密记忆。
                </span>
              </p>
            </template>

            <section v-if="!revoked && buddyCreateOpen" class="buddy-form" aria-label="新建伙伴">
              <label>
                伙伴名称
                <input
                  v-model="buddyForm.label"
                  type="text"
                  :disabled="!canMutate"
                  :placeholder="`最多 ${BUDDY_LABEL_MAX} 个 Unicode 字符`"
                  @input="buddyError = null"
                />
              </label>
              <label>
                伙伴人设
                <textarea
                  v-model="buddyForm.persona"
                  rows="3"
                  :disabled="!canMutate"
                  :placeholder="`最多 ${BUDDY_PERSONA_MAX_CHARS} 个字符；不继承任何旧伙伴的私密经历`"
                  @input="buddyError = null"
                />
              </label>
              <p class="muted">
                新实例 ID、私有域与归属由受信注册服务裁决；前端不提供 ID、路径、继承来源或权限字段。
              </p>
              <div class="panel-actions">
                <button type="button" class="btn quiet" :disabled="busy" @click="cancelBuddyCreate">取消</button>
                <button type="button" class="btn primary" :disabled="!canMutate" @click="submitBuddyCreate">
                  创建伙伴
                </button>
              </div>
            </section>

            <section v-if="!revoked && buddyRenameOpen && activeBuddy !== null" class="buddy-form" aria-label="伙伴改名">
              <label>
                新的伙伴名称
                <input
                  v-model="buddyRenameLabel"
                  type="text"
                  :disabled="!canMutate"
                  :placeholder="`最多 ${BUDDY_LABEL_MAX} 个 Unicode 字符`"
                  @input="buddyError = null"
                />
              </label>
              <p class="muted">改名只推进伙伴档案版本，不改变实例 ID、人设、模型配置或已有会话归属。</p>
              <div class="panel-actions">
                <button type="button" class="btn quiet" :disabled="busy" @click="cancelBuddyRename">取消</button>
                <button type="button" class="btn primary" :disabled="!canMutate" @click="submitBuddyRename">
                  保存名称
                </button>
              </div>
            </section>

            <p v-if="buddyError" class="chat-failure" role="alert">{{ buddyError }}</p>
            <p v-if="buddyNotice" class="panel-message" role="status">{{ buddyNotice }}</p>
          </div>
        </section>

        <section
          class="page"
          data-page-view="appearance"
          aria-labelledby="page-appearance-title"
          :hidden="activePage !== 'appearance'"
        >
          <header class="page-heading">
            <div>
              <h1 id="page-appearance-title">换一种可爱</h1>
              <p>为陪伴添一点新模样，还是熟悉的那个它。</p>
            </div>
            <div class="heading-actions">
              <button type="button" class="btn quiet" @click="showPet">
                <svg class="icon" aria-hidden="true"><use href="#mb-icon-window" /></svg>
                显示宠物
              </button>
              <button type="button" class="btn quiet" @click="togglePetPause">{{ pauseLabel }}</button>
            </div>
          </header>
          <div class="page-content">
            <div class="appearance-grid">
              <div class="pet-preview">
                <span class="preview-tag">插画 · 非当前素材预览</span>
                <svg class="pet-drawing" aria-hidden="true"><use href="#mb-cat" /></svg>
                <div class="pet-shadow" aria-hidden="true"></div>
                <strong>一点陪伴，一点可爱</strong>
                <small>实际外观以桌面宠物窗口为准</small>
              </div>
              <section class="card appearance-card">
                <div class="section-title">
                  <div>
                    <h2>素材与动作</h2>
                    <p>从本地素材包，为伙伴换个模样。</p>
                  </div>
                  <span class="pill neutral">仅本地</span>
                </div>
                <MaterialPanel
                  v-if="!revoked"
                  :context="materialContext"
                  :blocked-reason="materialBlockReason"
                  :page-hidden="activePage !== 'appearance'"
                />
                <p v-else class="muted">授权已撤回，素材面板不可用。</p>
              </section>
            </div>
            <p class="note appearance-note">
              <svg class="icon" aria-hidden="true"><use href="#mb-icon-info" /></svg>
              更换外观不会改变伙伴的身份、记忆或人设。素材选择只在本次运行中生效。
            </p>
          </div>
        </section>

        <section
          class="page"
          data-page-view="memory"
          aria-labelledby="page-memory-title"
          :hidden="activePage !== 'memory'"
        >
          <header class="page-heading">
            <div>
              <h1 id="page-memory-title">记住那些小事</h1>
              <p>属于 {{ activeBuddy ? buddyName(activeBuddy) : '当前伙伴' }} 的私密记忆，由你查看与整理。</p>
            </div>
            <div class="heading-actions">
              <button
                type="button"
                class="btn quiet"
                :disabled="panelBusy || managementExitConfirmation !== null"
                @click="requestPage('chat')"
              >
                返回聊天
              </button>
              <button
                type="button"
                class="btn quiet"
                :disabled="!panelActionsAllowed || panelBusy"
                @click="refreshMemories"
              >
                <svg class="icon" aria-hidden="true"><use href="#mb-icon-refresh" /></svg>
                刷新列表
              </button>
            </div>
          </header>
          <div class="page-content">
            <section v-if="activePanel === 'memory'" class="panel-shell" aria-label="记忆">
              <p v-if="panelNotice" class="panel-message" role="status">{{ panelNotice }}</p>
              <div class="panel-body">
                <div class="memory-toolbar">
                  <label class="grow">
                    检索
                    <input
                      v-model="memoryQuery"
                      type="search"
                      :disabled="!panelActionsAllowed || panelBusy"
                      @input="noteManagementInteraction"
                      @keyup.enter="searchMemories"
                    />
                  </label>
                  <button
                    type="button"
                    class="btn"
                    :disabled="!panelActionsAllowed || panelBusy"
                    @click="searchMemories"
                  >
                    检索
                  </button>
                </div>

                <div class="memory-layout">
                  <div class="memory-main">
                    <p v-if="memoryError" class="chat-failure" role="alert">{{ memoryError }}</p>
                    <p v-if="memoryNotice" class="panel-message" role="status">{{ memoryNotice }}</p>
                    <p class="memory-count">共 {{ memories.length }} 条获准展示的记忆</p>
                    <div v-if="memories.length === 0" class="empty-state">
                      <p class="empty-title">还没有可展示的记忆</p>
                      <p>属于当前伙伴的私密记忆会出现在这里。先在右侧记下一件小事，或刷新列表看看。</p>
                    </div>
                    <ul v-else class="memory-list">
                      <li v-for="memory in memories" :key="memory.id">
                        <template v-if="memoryEdit && memoryEdit.id === memory.id">
                          <div class="memory-editor">
                            <p v-if="memoryEditStale" class="chat-warning">
                              该记忆版本已变化；保存会被拒绝，当前编辑草稿仍保留。
                            </p>
                            <label>
                              编辑记忆
                              <textarea
                                v-model="memoryDraft"
                                rows="3"
                                :disabled="!canMutate"
                                @input="noteManagementInteraction"
                              />
                            </label>
                            <div class="panel-actions">
                              <button type="button" class="btn quiet" :disabled="panelBusy" @click="cancelMemoryEdit">
                                取消编辑
                              </button>
                              <button type="button" class="btn primary" :disabled="!canMutate" @click="saveMemory">
                                保存修改
                              </button>
                            </div>
                          </div>
                        </template>
                        <template v-else>
                          <div class="memory-text">
                            <p>{{ memory.text }}</p>
                            <span class="muted">版本 {{ memory.revision }}</span>
                          </div>
                          <div class="memory-actions">
                            <button type="button" class="btn quiet" :disabled="!canMutate" @click="beginEditMemory(memory)">
                              编辑
                            </button>
                            <template v-if="isMemoryDeleteArmed(deleteArm, memory)">
                              <button type="button" class="btn quiet" :disabled="panelBusy" @click="cancelDeleteMemory">
                                取消
                              </button>
                              <button
                                type="button"
                                class="btn danger"
                                :disabled="!canMutate"
                                @click="confirmDeleteMemory(memory)"
                              >
                                确认删除
                              </button>
                            </template>
                            <button
                              v-else
                              type="button"
                              class="btn quiet danger-link"
                              :disabled="!canMutate"
                              @click="requestDeleteMemory(memory)"
                            >
                              删除
                            </button>
                          </div>
                        </template>
                      </li>
                    </ul>
                  </div>

                  <div class="remember-form">
                    <h3>一件想记住的事</h3>
                    <label class="grow">
                      新增私密记忆
                      <textarea
                        v-model="rememberDraft"
                        rows="5"
                        :disabled="!panelActionsAllowed || panelBusy || commandUnconfirmed"
                        @input="noteManagementInteraction"
                      />
                    </label>
                    <button
                      type="button"
                      class="btn primary"
                      :disabled="!canMutate || rememberDraft.trim().length === 0"
                      @click="rememberMemory"
                    >
                      <svg class="icon" aria-hidden="true"><use href="#mb-icon-plus" /></svg>
                      记住
                    </button>
                    <p class="note">
                      <svg class="icon" aria-hidden="true"><use href="#mb-icon-lock" /></svg>
                      仅属于当前伙伴的私密记忆；不会自动共享给其他伙伴或服务。
                    </p>
                  </div>
                </div>
              </div>
            </section>
            <p v-else class="muted panel-closed-hint">
              记忆面板尚未打开；从侧栏重新进入「记忆」即可恢复。
            </p>
          </div>
        </section>

        <section
          class="page"
          data-page-view="model"
          aria-labelledby="page-model-title"
          :hidden="activePage !== 'model'"
        >
          <header class="page-heading">
            <div>
              <h1 id="page-model-title">让对话开始</h1>
              <p>为 {{ activeBuddy ? buddyName(activeBuddy) : '当前伙伴' }} 设置模型、人设与发送许可。</p>
            </div>
            <div class="heading-actions">
              <span class="pill neutral">
                <svg class="icon icon-sm" aria-hidden="true"><use href="#mb-icon-lock" /></svg>
                {{ externalTransmissionAllowed ? '已允许发送' : '尚未允许发送' }}
              </span>
              <button
                type="button"
                class="btn quiet"
                :disabled="panelBusy || managementExitConfirmation !== null"
                @click="requestPage('chat')"
              >
                返回聊天
              </button>
            </div>
          </header>
          <div class="page-content">
            <section v-if="activePanel === 'presets'" class="panel-shell" aria-label="模型设置">
              <p v-if="panelNotice" class="panel-message" role="status">{{ panelNotice }}</p>
              <div class="panel-body">
                <div class="panel-status">
                  <span>当前预设：{{ activePreset || '未选择' }}</span>
                  <span>{{ transmissionStatusText(externalTransmissionAllowed) }}</span>
                  <span>{{ credentialStatusText(modelSettings) }}</span>
                  <span v-if="!persistenceEnabled">本地保存不可用</span>
                </div>
                <p v-if="modelChangeHint" class="chat-warning">{{ modelChangeHint }}</p>
                <p v-if="modelApplyNotice" class="panel-message" role="status">{{ modelApplyNotice }}</p>

                <section class="panel-block">
                  <h3>可用预设</h3>
                  <div v-if="presets.length === 0" class="empty-state">
                    <p class="empty-title">还没有可用预设</p>
                    <p>桌面宿主尚未返回预设；可以直接在下方填写自定义连接。</p>
                  </div>
                  <ul v-else class="preset-list">
                    <li
                      v-for="preset in presets"
                      :key="preset.id"
                      :class="{ selected: preset.id === activePreset }"
                    >
                      <div class="preset-text">
                        <strong>{{ preset.label || preset.id }}</strong>
                        <p class="muted">{{ preset.provider }} · {{ preset.model }}</p>
                        <p class="muted endpoint">{{ preset.endpoint }}</p>
                      </div>
                      <button
                        type="button"
                        class="btn"
                        :disabled="!canMutate || preset.id === activePreset"
                        @click="selectPreset(preset)"
                      >
                        {{ preset.id === activePreset ? '当前预设' : '使用此预设' }}
                      </button>
                    </li>
                  </ul>
                </section>

                <form class="panel-form" @submit.prevent="applyModelSettings">
                  <h3>模型配置与发送许可</h3>
                  <label>
                    端点
                    <input
                      v-model="modelForm.endpoint"
                      type="url"
                      placeholder="https://…/v1/chat/completions"
                      :disabled="!canMutate"
                      @input="onModelIdentityInput"
                    />
                  </label>
                  <label>
                    模型
                    <input
                      v-model="modelForm.model"
                      type="text"
                      :disabled="!canMutate"
                      @input="onModelIdentityInput"
                    />
                  </label>
                  <label>
                    人设
                    <textarea
                      v-model="modelForm.persona"
                      rows="3"
                      :disabled="!canMutate"
                      @input="noteModelInteraction"
                    />
                  </label>
                  <label class="checkbox-row">
                    <input
                      v-model="modelForm.allowTransmission"
                      type="checkbox"
                      :disabled="!canMutate"
                      @change="onTransmissionChange"
                    />
                    允许本次请求向端点发送人设、获准私密记忆和当前会话内容
                  </label>
                  <label>
                    API Key
                    <input
                      v-model="modelForm.apiKeyInput"
                      type="password"
                      autocomplete="off"
                      :disabled="!canMutate"
                      @input="noteModelInteraction"
                      :placeholder="
                        modelSettings?.hasApiKey
                          ? '已保存本次进程凭据；留空表示保留'
                          : '未配置；留空表示不使用'
                      "
                    />
                  </label>
                  <label class="checkbox-row">
                    <input
                      v-model="modelForm.clearApiKey"
                      type="checkbox"
                      :disabled="!canMutate || modelForm.apiKeyInput.length > 0"
                      @change="noteModelInteraction"
                    />
                    本次不使用 / 清除 API Key（以空串显式替换）
                  </label>
                  <p class="muted">API Key 只用于本次桌面宿主进程，不写入快照或本地存储；应用成功后密码框会清空。</p>
                  <p class="muted">配置 API Key 不等于允许外传；许可仅覆盖上方列出的内容。</p>
                  <div class="panel-actions">
                    <button
                      type="button"
                      class="btn quiet"
                      :disabled="!canMutate || !modelPanelDirty"
                      @click="resetModelForm"
                    >
                      放弃修改
                    </button>
                    <button type="submit" class="btn primary" :disabled="!canMutate">应用配置</button>
                  </div>
                </form>
              </div>
            </section>
            <p v-else class="muted panel-closed-hint">
              模型设置面板尚未打开；从侧栏重新进入「模型设置」即可恢复。
            </p>
          </div>
        </section>

        <section
          class="page"
          data-page-view="diagnostics"
          aria-labelledby="page-diagnostics-title"
          :hidden="activePage !== 'diagnostics'"
        >
          <header class="page-heading">
            <div>
              <h1 id="page-diagnostics-title">看看运行情况</h1>
              <p>需要排查问题时，在这里查看获准展示的事件。</p>
            </div>
            <div class="heading-actions">
              <button
                type="button"
                class="btn quiet"
                :disabled="panelBusy || managementExitConfirmation !== null"
                @click="requestPage('chat')"
              >
                返回聊天
              </button>
              <button
                type="button"
                class="btn quiet"
                :disabled="!panelActionsAllowed || panelBusy"
                @click="refreshDiagnostics"
              >
                <svg class="icon" aria-hidden="true"><use href="#mb-icon-refresh" /></svg>
                刷新诊断
              </button>
            </div>
          </header>
          <div class="page-content">
            <section v-if="activePanel === 'diagnostics'" class="panel-shell" aria-label="运行诊断">
              <p v-if="panelNotice" class="panel-message" role="status">{{ panelNotice }}</p>
              <div class="panel-body">
                <p class="muted">只展示宿主授权的有界诊断；Gap 由宿主提示显式说明。流程完成与消息实际写入分别呈现。</p>
                <div v-if="diagnostics.length === 0" class="empty-state">
                  <p class="empty-title">还没有可展示的诊断事件</p>
                  <p>本次运行暂无获准展示的事件；聊过几句或点「刷新诊断」后再来看看。</p>
                </div>
                <ul v-else class="diagnostic-list">
                  <li v-for="item in diagnostics" :key="item.sequence">
                    <span class="diag-seq">#{{ item.sequence }}</span>
                    <span>{{ item.run }}</span>
                    <span>{{ item.event }}</span>
                    <span class="diag-status">{{ item.status }}</span>
                  </li>
                </ul>
                <div class="divider"></div>
                <div class="diagnostics-foot">
                  <p class="muted">
                    可使用侧栏「退出应用」结束本次使用，或「撤回授权」清空当前展示与草稿；撤权后不能在此重新授权。
                  </p>
                </div>
              </div>
            </section>
            <p v-else class="muted panel-closed-hint">
              运行诊断面板尚未打开；从侧栏重新进入「运行诊断」即可恢复。
            </p>
          </div>
        </section>

        <div v-if="revokeArmed" class="inline-confirm" role="alertdialog" aria-label="撤回授权确认">
          <p>撤回后将清空界面展示的聊天与草稿，并且不能在此重新授权。</p>
          <div class="inline-confirm-actions">
            <button type="button" class="btn quiet" :disabled="busy" @click="revokeArmed = false">取消</button>
            <button type="button" class="btn danger" :disabled="busy" @click="confirmRevoke">
              确认撤回
            </button>
          </div>
        </div>

        <div v-if="archiveArmed" class="inline-confirm" role="alertdialog" aria-label="归档会话确认">
          <p>
            归档后仍可显式选择查看，但不能发送或继续编辑草稿；不会回收原文或删除会话，本阶段不提供解归档。
          </p>
          <div class="inline-confirm-actions">
            <button type="button" class="btn quiet" :disabled="busy" @click="archiveArmed = false">取消</button>
            <button type="button" class="btn danger" :disabled="busy" @click="confirmArchiveSession">
              确认归档
            </button>
          </div>
        </div>

        <div v-if="closeConfirmation" class="dialog-backdrop">
          <div
            class="dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="close-dialog-title"
            aria-describedby="close-dialog-body"
          >
            <h2 id="close-dialog-title">{{ closeDialogTitle(closeIntent) }}</h2>
            <p id="close-dialog-body">{{ closeDialogBody(closeIntent) }}</p>
            <div class="dialog-actions">
              <button type="button" class="btn quiet" :disabled="busy" @click="resolveClose(false)">取消</button>
              <button type="button" class="btn danger" :disabled="busy" @click="resolveClose(true)">
                {{ closeDialogConfirmLabel(closeIntent) }}
              </button>
            </div>
          </div>
        </div>

        <div v-if="managementExitConfirmation !== null" class="dialog-backdrop">
          <div
            class="dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="management-exit-title"
            aria-describedby="management-exit-body"
          >
            <h2 id="management-exit-title">
              {{ closeDialogTitle(managementExitConfirmation.intent) }}
            </h2>
            <p id="management-exit-body">
              {{ managementExitBody(snapshot?.managementDraft.dirty === true, managementExitConfirmation.intent) }}
            </p>
            <div class="dialog-actions">
              <button type="button" class="btn quiet" :disabled="busy" @click="resolveManagementExit(false)">取消</button>
              <button type="button" class="btn danger" :disabled="busy" @click="resolveManagementExit(true)">
                {{ managementExitConfirmLabel(snapshot?.managementDraft.dirty === true) }}
              </button>
            </div>
          </div>
        </div>

        <div v-if="pendingPanelLeave !== null" class="dialog-backdrop">
          <div
            class="dialog"
            role="dialog"
            aria-modal="true"
            aria-labelledby="leave-dialog-title"
            aria-describedby="leave-dialog-body"
          >
            <h2 id="leave-dialog-title">放弃未保存的编辑？</h2>
            <p id="leave-dialog-body">
              {{ pendingLeaveDescription }}选择“继续编辑”会保留当前草稿和页面。
            </p>
            <div class="dialog-actions">
              <button type="button" class="btn quiet" @click="cancelPanelLeave">继续编辑</button>
              <button type="button" class="btn danger" @click="confirmPanelLeave">放弃并离开</button>
            </div>
          </div>
        </div>
        </template>
      </main>
    </div>
  </div>
</template>

<style scoped>
.main-view {
  display: flex;
  flex-direction: column;
  width: 100%;
  height: 100%;
  background: var(--paper);
  color: var(--ink);
}

.svg-defs {
  position: absolute;
  width: 0;
  height: 0;
  overflow: hidden;
}

.icon {
  width: 19px;
  height: 19px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.65;
  stroke-linecap: round;
  stroke-linejoin: round;
  flex-shrink: 0;
}

.icon-sm {
  width: 13px;
  height: 13px;
}

.center-state {
  flex: 1;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  gap: 8px;
  text-align: center;
  padding: 24px;
}

.center-title {
  font-size: 17px;
  font-weight: 600;
}

/* ---------- 应用骨架：侧栏 + 工作区 ---------- */

.app-shell {
  display: grid;
  grid-template-columns: 204px minmax(0, 1fr);
  flex: 1;
  height: 100%;
  min-height: 380px;
  overflow: hidden;
}

.sidebar {
  background: var(--sidebar);
  border-right: 1px solid var(--line);
  padding: 25px 17px 15px;
  display: flex;
  flex-direction: column;
  min-height: 0;
  overflow: auto;
}

.brand {
  display: flex;
  gap: 10px;
  align-items: center;
  margin: 0 9px 32px;
  min-width: 0;
}

.brand-mark {
  width: 36px;
  height: 36px;
  border-radius: 12px;
  background: var(--moss);
  display: grid;
  place-items: center;
  color: #fff;
  flex-shrink: 0;
}

.brand-mark .icon {
  width: 24px;
  height: 24px;
}

/* flex 子项允许收缩：长副标题在窄侧栏内省略，而不是把侧栏撑出横向滚动。 */
.brand-name {
  min-width: 0;
}

.brand-name strong {
  display: block;
  font-size: 18px;
  letter-spacing: -0.7px;
  line-height: 1.3;
}

.brand-name small {
  display: block;
  font-size: 9px;
  letter-spacing: 0.12em;
  color: #77836f;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
}

.nav-label {
  margin: 0 13px 11px;
  font-size: 10px;
  letter-spacing: 0.15em;
  color: #87917f;
}

.nav-label + .navigation {
  margin-bottom: 4px;
}

.navigation {
  display: grid;
  gap: 6px;
}

.nav-item {
  display: flex;
  gap: 12px;
  align-items: center;
  width: 100%;
  padding: 12px 13px;
  border: 0;
  border-radius: 10px;
  background: none;
  text-align: left;
  color: #727f70;
  font-size: 13px;
  font-weight: 500;
  transition: background 0.16s, color 0.16s, box-shadow 0.16s;
}

.nav-item:hover {
  background: #e7ede1;
  color: var(--moss);
}

.nav-item[aria-current='page'] {
  background: var(--moss);
  color: #fff;
  box-shadow: 0 3px 8px rgba(50, 91, 60, 0.07);
}

.nav-arrow {
  margin-left: auto;
  font-size: 16px;
  opacity: 0.7;
}

.sidebar-bottom {
  margin-top: auto;
  padding-top: 28px;
}

.companion-mini {
  display: flex;
  gap: 10px;
  align-items: center;
  padding: 13px 9px;
  border-top: 1px solid #dfe5d9;
}

.companion-mini strong {
  display: block;
  font-size: 12px;
  overflow-wrap: anywhere;
}

.companion-mini small {
  display: block;
  color: var(--muted);
  font-size: 10px;
}

.avatar {
  display: grid;
  place-items: center;
  width: 35px;
  height: 35px;
  border-radius: 11px;
  background: #e4eadc;
  color: var(--moss);
  flex-shrink: 0;
  font-size: 14px;
  font-weight: 600;
  overflow: hidden;
}

.avatar svg {
  width: 27px;
  height: 27px;
}

.avatar-sm {
  width: 30px;
  height: 30px;
  border-radius: 10px;
}

.avatar-sm svg {
  width: 23px;
  height: 23px;
}

.dot {
  width: 6px;
  height: 6px;
  background: #719365;
  border-radius: 50%;
  display: inline-block;
  flex-shrink: 0;
}

.grow {
  flex: 1;
  min-width: 0;
}

.sidebar-tools {
  display: flex;
  justify-content: space-between;
  gap: 6px;
  padding: 8px 3px 0;
}

.sidebar-tools button {
  display: flex;
  align-items: center;
  gap: 6px;
  border: 0;
  background: none;
  font-size: 11px;
  color: #7c8578;
  padding: 5px;
  border-radius: 8px;
  white-space: nowrap;
}

.sidebar-tools button:hover {
  background: var(--moss-soft);
  color: var(--moss);
}

.sidebar-tools .icon {
  width: 14px;
  height: 14px;
}

.sidebar-revoke {
  display: flex;
  align-items: center;
  gap: 8px;
  width: 100%;
  padding: 7px 9px;
  border: 0;
  border-radius: 8px;
  background: none;
  color: var(--muted);
  font-size: 11px;
}

.sidebar-revoke .icon {
  width: 14px;
  height: 14px;
}

.sidebar-revoke:not(:disabled):hover {
  color: var(--danger);
  background: var(--danger-soft);
}

.workspace {
  min-width: 0;
  display: flex;
  flex-direction: column;
  min-height: 0;
}

.topbar {
  min-height: 51px;
  flex-shrink: 0;
  padding: 0 30px;
  border-bottom: 1px solid var(--line);
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
}

.breadcrumb {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 11px;
  color: var(--muted);
  white-space: nowrap;
}

.crumb-root,
.slash {
  color: #9aa394;
}

.chat-meta {
  display: flex;
  align-items: center;
  gap: 8px;
  flex-wrap: wrap;
  justify-content: flex-end;
  font-size: 11px;
  min-width: 0;
}

.offline-note {
  color: var(--muted);
}

.status-pill,
.revoked-pill,
.pet-pill,
.pending-pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 3px 9px;
  border-radius: 999px;
  white-space: nowrap;
}

.status-pill::before {
  content: '';
  width: 6px;
  height: 6px;
  border-radius: 50%;
  background: currentColor;
}

.tone-ok {
  background: var(--moss-soft);
  color: var(--moss);
}

.tone-busy {
  background: var(--warn-soft);
  color: var(--warn);
}

.tone-warn {
  background: var(--warn-soft);
  color: var(--warn);
}

.tone-danger {
  background: var(--danger-soft);
  color: var(--danger);
}

.revoked-pill {
  background: var(--danger-soft);
  color: var(--danger);
  font-weight: 600;
}

.pet-pill {
  background: #f0f2ed;
  color: #69756b;
}

.pending-pill {
  background: var(--warn-soft);
  color: var(--warn);
}

/* ---------- 跨页横幅 ---------- */

.recovery-banner,
.chat-warning,
.chat-failure,
.error-banner,
.archived-banner {
  margin: 0;
  padding: 10px 30px;
  font-size: 12px;
  line-height: 1.7;
  overflow-wrap: anywhere;
}

.recovery-banner {
  background: var(--moss-soft);
  color: var(--moss-deep);
  border-bottom: 1px solid var(--line);
}

.chat-warning {
  background: var(--warn-soft);
  color: var(--warn);
}

.chat-failure {
  background: var(--danger-soft);
  color: var(--danger);
}

.archived-banner {
  background: #f0f2ed;
  color: #69756b;
  border-top: 1px solid var(--line);
  border-bottom: 1px solid var(--line);
}

.error-banner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  background: var(--danger-soft);
  color: var(--danger);
  border-bottom: 1px solid var(--line);
}

.error-banner span {
  flex: 1;
  min-width: 0;
}

.ghost {
  border: 1px solid currentColor;
  background: none;
  border-radius: 8px;
  padding: 3px 10px;
  font-size: 11px;
  white-space: nowrap;
}

/* ---------- 页面骨架 ---------- */

.page {
  display: flex;
  flex: 1;
  flex-direction: column;
  min-height: 0;
  min-width: 0;
}

.page-heading {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 16px;
  padding: 25px 30px 22px;
  flex-shrink: 0;
}

.page-heading h1 {
  margin: 0;
  font-size: 24px;
  letter-spacing: -0.7px;
  line-height: 1.4;
  font-weight: 600;
}

/* 切页后焦点移动到标题供读屏播报；与样稿一致不画可见焦点框。 */
.page-heading h1:focus,
.page-heading h1:focus-visible {
  outline: none;
}

.page-heading p {
  margin: 5px 0 0;
  color: var(--muted);
  font-size: 12px;
}

.chat-heading {
  padding-bottom: 20px;
}

.heading-actions {
  display: flex;
  align-items: center;
  gap: 9px;
  flex-wrap: wrap;
  justify-content: flex-end;
  min-width: 0;
}

/* 长伙伴名按钮在窄窗内换行显示，而不是把页头撑出容器。 */
.heading-actions .btn {
  max-width: 100%;
  min-width: 0;
}

.heading-actions .btn span {
  white-space: normal;
  overflow-wrap: anywhere;
  text-align: left;
}

.page-content {
  overflow: auto;
  padding: 0 30px 30px;
  flex: 1;
  min-height: 0;
}

.btn {
  display: inline-flex;
  align-items: center;
  justify-content: center;
  gap: 8px;
  min-height: 38px;
  padding: 8px 14px;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--surface);
  color: var(--ink);
  white-space: nowrap;
  font-size: 12px;
  font-weight: 600;
  transition: background 0.16s, color 0.16s, box-shadow 0.16s;
}

.btn:hover {
  background: #f0f4ed;
  border-color: #cdd9c8;
}

.btn.primary {
  background: var(--moss);
  border-color: var(--moss);
  color: #fff;
}

.btn.primary:hover {
  background: var(--moss-deep);
}

.btn.quiet {
  background: transparent;
}

.btn.danger {
  color: var(--danger);
}

button {
  font: inherit;
  color: inherit;
}

button:disabled {
  cursor: default;
  opacity: 0.45;
}

/* 管理与表单按钮统一走 .btn；这里只保留图标按钮等少量例外样式。 */

.icon-btn {
  display: grid;
  place-items: center;
  width: 32px;
  height: 32px;
  border: 0;
  border-radius: 8px;
  background: none;
  color: var(--muted);
}

.icon-btn:hover {
  background: var(--moss-soft);
  color: var(--moss);
}

.inline-link {
  border: 0;
  background: none;
  padding: 0;
  color: var(--moss);
  font-size: inherit;
  text-decoration: underline;
  text-underline-offset: 3px;
}

.pill {
  display: inline-flex;
  align-items: center;
  gap: 6px;
  padding: 4px 9px;
  border-radius: 6px;
  background: var(--moss-soft);
  color: var(--moss);
  font-size: 11px;
  white-space: nowrap;
}

.pill.neutral {
  background: #f0f2ed;
  color: #69756b;
}

.micro {
  font-size: 11px;
  letter-spacing: 0.08em;
}

.muted {
  color: var(--muted);
}

.row {
  display: flex;
  align-items: center;
  gap: 10px;
  min-width: 0;
}

.note {
  color: var(--muted);
  font-size: 11px;
  display: flex;
  align-items: flex-start;
  gap: 8px;
  line-height: 1.8;
}

.note .icon {
  width: 15px;
  height: 15px;
  margin-top: 3px;
}

.section-label {
  display: flex;
  align-items: center;
  gap: 10px;
  font-size: 11px;
  color: var(--muted);
  margin: 0 0 13px;
}

.section-label::after {
  height: 1px;
  background: var(--line);
  content: '';
  flex: 1;
}

.card {
  border: 1px solid var(--line);
  border-radius: 16px;
  background: var(--surface);
  padding: 24px;
}

.section-title {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  margin-bottom: 19px;
}

.section-title h2 {
  margin: 0;
  font-size: 17px;
  letter-spacing: -0.2px;
}

.section-title p {
  margin: 5px 0 0;
  font-size: 12px;
  color: var(--muted);
}

.divider {
  height: 1px;
  background: var(--line);
}

/* ---------- 聊天页 ---------- */

.chat-body {
  display: grid;
  grid-template-columns: 200px minmax(0, 1fr);
  flex: 1;
  min-height: 0;
  border-top: 1px solid var(--line);
}

.sessions {
  padding: 22px 14px 16px 18px;
  border-right: 1px solid var(--line);
  display: flex;
  flex-direction: column;
  min-height: 0;
}

.session-top {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 0 6px 16px;
}

.session-top strong {
  font-size: 11px;
  color: var(--muted);
  font-weight: 500;
  letter-spacing: 0.1em;
}

.session-scroll {
  overflow: auto;
  flex: 1;
  min-height: 0;
}

.session-chip {
  display: flex;
  gap: 9px;
  padding: 13px 11px;
  border: 0;
  border-radius: 11px;
  background: none;
  width: 100%;
  text-align: left;
  margin-bottom: 5px;
  align-items: flex-start;
  color: var(--ink);
}

.session-chip .icon {
  width: 16px;
  height: 16px;
  color: #84927a;
  margin-top: 3px;
}

.session-chip.active {
  background: #e9eee3;
}

.session-chip:hover {
  background: #edf1e8;
}

.session-text {
  min-width: 0;
}

.session-name {
  display: block;
  font-size: 12px;
  font-weight: 500;
  overflow-wrap: anywhere;
}

.session-chip.archived .session-name {
  color: var(--muted);
}

.session-sub {
  display: flex;
  gap: 6px;
  align-items: center;
  margin-top: 3px;
}

.session-flag {
  font-size: 10px;
  color: #8a6a33;
  background: var(--warn-soft);
  border-radius: 5px;
  padding: 0 5px;
}

.session-status {
  font-size: 10px;
  color: #899282;
}

.tone-ok.session-status {
  color: #6f8b62;
}

.tone-busy.session-status,
.tone-warn.session-status {
  color: var(--warn);
}

.tone-danger.session-status {
  color: var(--danger);
}

.tone-muted.session-status {
  color: var(--muted);
}

.session-empty {
  font-size: 11px;
  color: var(--muted);
  line-height: 1.7;
  display: block;
  padding: 4px 6px;
}

.session-bottom {
  padding: 15px 6px 0;
  font-size: 10px;
  color: #8d9787;
  border-top: 1px solid var(--line);
  margin: 12px 0 0;
}

.conversation {
  display: flex;
  flex-direction: column;
  min-height: 0;
  min-width: 0;
  background: linear-gradient(180deg, #fffefb 0%, #f9faf6 100%);
}

.conversation-head {
  padding: 15px 26px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  border-bottom: 1px solid #edf0e7;
  gap: 10px;
  flex-shrink: 0;
}

/* 长伙伴名在这里收缩换行，右侧「归档会话」按钮保持可达。 */
.conversation-head .row {
  flex: 1;
  min-width: 0;
}

.conversation-head strong {
  font-size: 13px;
  overflow-wrap: anywhere;
}

.conversation-head small {
  font-size: 10px;
  color: #829076;
  display: flex;
  align-items: center;
  gap: 5px;
}

.messages {
  flex: 1;
  min-height: 0;
  overflow: auto;
  padding: 28px 30px 20px;
}

.empty-state {
  display: grid;
  gap: 6px;
  align-content: center;
  justify-items: center;
  text-align: center;
  padding: 30px 20px;
  color: var(--muted);
  font-size: 12px;
  min-height: 160px;
}

.empty-title {
  font-size: 14px;
  font-weight: 600;
  color: var(--ink);
  margin: 0;
}

.revoked-state {
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 16px;
}

.message {
  display: flex;
  align-items: flex-start;
  gap: 10px;
  margin-bottom: 23px;
}

.message-body {
  max-width: min(560px, 85%);
  min-width: 0;
}

.message-meta {
  margin: 0 0 6px 2px;
  font-size: 10px;
  color: #919a8a;
}

.bubble {
  padding: 14px 17px;
  border-radius: 3px 15px 15px 15px;
  background: var(--surface);
  border: 1px solid #e8ecdf;
  line-height: 1.9;
  font-size: 13px;
  box-shadow: 0 2px 4px rgba(46, 79, 40, 0.06);
}

.bubble .text {
  margin: 0;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.from-user {
  justify-content: flex-end;
}

.from-user .message-meta {
  text-align: right;
}

.from-user .bubble {
  background: #e9efdf;
  border-color: #e2e9d6;
  border-radius: 15px 3px 15px 15px;
}

.from-system .bubble {
  background: #f4f6ef;
  border-style: dashed;
  color: var(--muted);
}

.composer-wrap {
  padding: 0 26px 19px;
  flex-shrink: 0;
}

.composer {
  display: block;
  border: 1px solid #d8e0d0;
  border-radius: 15px;
  background: var(--surface);
  padding: 12px 15px 11px;
  box-shadow: 0 4px 16px rgba(62, 80, 37, 0.02);
}

.composer:focus-within {
  border-color: #8da37e;
  box-shadow: 0 0 0 3px #edf1e7;
}

.composer textarea {
  display: block;
  width: 100%;
  resize: none;
  border: 0;
  outline: 0 !important;
  box-shadow: none;
  min-height: 56px;
  max-height: 160px;
  padding: 3px 1px;
  font-size: 13px;
  background: none;
  color: var(--ink);
}

.composer textarea::placeholder {
  color: #a0a894;
}

.composer textarea:disabled {
  opacity: 0.6;
}

.composer-bottom {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 8px;
}

.composer-bottom small {
  font-size: 10px;
  color: #98a18e;
}

.composer .btn {
  padding: 7px 13px;
  min-height: 33px;
}

.composer .icon {
  width: 14px;
  height: 14px;
}

.composer-caption {
  font-size: 10px;
  color: #8d9785;
  margin: 10px 0 0;
  text-align: center;
}

/* ---------- 伙伴页 ---------- */

.buddy-hero {
  background: #eaf0e1;
  border: 1px solid #dce6d2;
  border-radius: 18px;
  padding: 27px 30px;
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 20px;
  margin-bottom: 25px;
  overflow: hidden;
}

.buddy-hero h2 {
  margin: 9px 0 7px;
  font-size: 25px;
  font-weight: 500;
  letter-spacing: -0.6px;
  line-height: 1.4;
}

.buddy-hero p {
  margin: 0;
  font-size: 12px;
  color: #7c886e;
  max-width: 380px;
}

.hero-cat {
  width: 140px;
  height: 140px;
  flex-shrink: 0;
  transform: rotate(-6deg);
}

.buddy-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.buddy-card {
  padding: 22px;
  min-width: 0;
}

.buddy-card.selected,
.buddy-card.active {
  border-color: #9cb48c;
  box-shadow: inset 0 0 0 1px #bed0af;
}

.buddy-card h3 {
  margin: 0;
  font-size: 16px;
}

.buddy-card .row {
  align-items: flex-start;
}

.buddy-card .pill {
  margin-left: auto;
}

/* 实例 ID 与档案/人设/模型修订收进可展开详情，日常视图不再直出编号。 */
.buddy-detail {
  margin-top: 14px;
  border-top: 1px dashed var(--line);
  padding-top: 8px;
  min-width: 0;
}

.buddy-detail summary {
  cursor: pointer;
  font-size: 11px;
  color: var(--muted);
  padding: 2px 0;
  width: fit-content;
}

.buddy-detail summary:hover {
  color: var(--moss);
}

.buddy-detail[open] summary {
  margin-bottom: 6px;
}

.buddy-detail-list {
  margin: 0;
  display: grid;
  gap: 5px;
}

.buddy-detail-line {
  display: flex;
  gap: 10px;
  font-size: 11px;
  min-width: 0;
}

.buddy-detail-line dt {
  color: var(--muted);
  flex-shrink: 0;
}

.buddy-detail-line dd {
  margin: 0;
  overflow-wrap: anywhere;
  color: var(--ink);
}

.buddy-revisions {
  font-size: 11px;
  color: var(--muted);
}

.buddy-detail-note {
  margin: 8px 0 0;
  font-size: 10px;
  line-height: 1.7;
}

.buddy-card-foot {
  border-top: 1px solid var(--line);
  padding-top: 15px;
  margin-top: 17px;
  display: flex;
  justify-content: flex-end;
  align-items: center;
  gap: 10px;
  flex-wrap: wrap;
}

.buddy-card-foot .btn {
  min-height: 31px;
  padding: 4px 11px;
}

.buddy-identity {
  margin: 22px 0 0;
}

.buddy-form {
  display: grid;
  gap: 12px;
  margin-top: 22px;
  border: 1px solid var(--line);
  border-radius: 16px;
  background: var(--surface);
  padding: 22px;
}

.buddy-form label {
  display: grid;
  gap: 7px;
  font-size: 12px;
  font-weight: 600;
}

.buddy-form input,
.buddy-form textarea {
  font-weight: 400;
  width: 100%;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--surface);
  padding: 10px 12px;
  color: var(--ink);
  min-width: 0;
}

.buddy-form textarea {
  resize: vertical;
}

/* ---------- 外观页 ---------- */

.appearance-grid {
  display: grid;
  grid-template-columns: minmax(240px, 0.9fr) minmax(0, 1.1fr);
  gap: 22px;
  align-items: start;
}

.pet-preview {
  min-height: 320px;
  display: flex;
  flex-direction: column;
  align-items: center;
  justify-content: center;
  background: radial-gradient(ellipse at 50% 65%, #e5edda 0%, #f2f5eb 52%, #f8faf3 100%);
  border: 1px solid #e2e9d9;
  border-radius: 18px;
  position: relative;
  overflow: hidden;
  padding: 24px;
  text-align: center;
}

.preview-tag {
  position: absolute;
  top: 18px;
  left: 19px;
  color: #8a957f;
  font-size: 10px;
  letter-spacing: 0.08em;
}

.pet-drawing {
  width: 190px;
  height: 190px;
  filter: drop-shadow(0 14px 12px rgba(109, 130, 83, 0.1));
}

.pet-shadow {
  width: 114px;
  height: 12px;
  background: #d6dfca;
  filter: blur(5px);
  border-radius: 50%;
  margin-top: -12px;
}

.pet-preview strong {
  font-size: 14px;
  font-weight: 500;
  margin-top: 23px;
}

.pet-preview small {
  font-size: 10px;
  color: #8c987e;
  margin-top: 5px;
}

.appearance-card {
  min-width: 0;
}

.appearance-note {
  padding: 19px 1px;
  margin: 0;
}

/* ---------- 面板页（记忆 / 模型 / 诊断） ---------- */

/* 页面级 h1 由 page-heading 承担，面板本身不再重复大标题；
   返回聊天入口统一放在页头 heading-actions。 */
.panel-shell {
  display: grid;
  gap: 14px;
}

.panel-message {
  margin: 0;
  font-size: 12px;
  line-height: 1.7;
  overflow-wrap: anywhere;
}

.panel-body {
  display: grid;
  gap: 16px;
  min-width: 0;
}

.panel-closed-hint {
  margin: 0;
}

.panel-status {
  display: flex;
  flex-wrap: wrap;
  gap: 8px 18px;
  font-size: 12px;
  color: var(--muted);
  background: #f2f4ed;
  border: 1px solid var(--line);
  border-radius: 12px;
  padding: 12px 15px;
}

.panel-block h3,
.panel-form h3 {
  margin: 0 0 12px;
  font-size: 14px;
}

.preset-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: grid;
  gap: 10px;
}

.preset-list li {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
  border: 1px solid var(--line);
  border-radius: 12px;
  padding: 14px 16px;
  background: var(--surface);
}

.preset-list li.selected {
  border-color: #9cb48c;
  background: #f4f7ef;
}

.preset-list .btn {
  flex-shrink: 0;
  min-height: 34px;
  padding: 6px 12px;
  font-size: 11px;
}

.preset-text {
  min-width: 0;
}

.preset-text strong {
  font-size: 13px;
}

.preset-text p {
  margin: 3px 0 0;
  font-size: 11px;
}

.preset-text .endpoint {
  overflow-wrap: anywhere;
}

.panel-form {
  display: grid;
  gap: 14px;
  border: 1px solid var(--line);
  border-radius: 16px;
  background: var(--surface);
  padding: 20px;
}

.panel-form label {
  display: grid;
  gap: 7px;
  font-size: 12px;
  font-weight: 600;
}

.panel-form input,
.panel-form textarea {
  font-weight: 400;
  width: 100%;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--surface);
  padding: 10px 12px;
  color: var(--ink);
  min-width: 0;
}

.panel-form textarea {
  resize: vertical;
}

.checkbox-row {
  display: flex;
  align-items: flex-start;
  gap: 9px;
  font-size: 12px;
  font-weight: 400;
  line-height: 1.8;
}

.checkbox-row input {
  accent-color: var(--moss);
  margin: 5px 0 0;
  width: auto;
}

.panel-actions {
  display: flex;
  align-items: center;
  justify-content: flex-end;
  gap: 9px;
  flex-wrap: wrap;
}

.memory-toolbar {
  display: flex;
  gap: 9px;
  align-items: end;
}

.memory-toolbar label {
  display: grid;
  gap: 7px;
  font-size: 12px;
  font-weight: 600;
  flex: 1;
  min-width: 0;
}

.memory-toolbar input {
  font-weight: 400;
  width: 100%;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--surface);
  padding: 10px 12px;
  color: var(--ink);
  min-width: 0;
}

.memory-layout {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 265px;
  gap: 22px;
  align-items: start;
}

.memory-main {
  min-width: 0;
  display: grid;
  gap: 12px;
}

.memory-count {
  margin: 0;
  font-size: 11px;
  color: var(--muted);
}

.memory-list {
  list-style: none;
  margin: 0;
  padding: 0;
  display: grid;
  gap: 12px;
}

.memory-list li {
  border: 1px solid var(--line);
  border-radius: 14px;
  background: var(--surface);
  padding: 18px 20px;
}

.memory-text p {
  margin: 0;
  font-size: 13px;
  line-height: 1.9;
  overflow-wrap: anywhere;
}

.memory-text .muted {
  font-size: 10px;
}

.memory-actions {
  display: flex;
  justify-content: flex-end;
  gap: 6px;
  margin-top: 12px;
}

.memory-actions .btn {
  min-height: 30px;
  padding: 4px 10px;
  font-size: 11px;
}

.memory-actions .danger,
.memory-actions .danger-link {
  color: var(--danger);
  border-color: var(--danger-soft);
  background: var(--surface);
}

.memory-actions .danger:hover,
.memory-actions .danger-link:hover {
  background: var(--danger-soft);
  border-color: #e3c3b8;
}

.memory-editor {
  display: grid;
  gap: 10px;
}

.memory-editor label {
  display: grid;
  gap: 7px;
  font-size: 12px;
  font-weight: 600;
}

.memory-editor textarea {
  font-weight: 400;
  width: 100%;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--surface);
  padding: 10px 12px;
  color: var(--ink);
  resize: vertical;
  min-width: 0;
}

.remember-form {
  border: 1px solid #e4e9d8;
  border-radius: 16px;
  background: #f2f4ea;
  padding: 21px;
  display: grid;
  gap: 12px;
  position: sticky;
  top: 0;
}

.remember-form h3 {
  margin: 0;
  font-size: 14px;
}

.remember-form label {
  display: grid;
  gap: 7px;
  font-size: 12px;
  font-weight: 600;
}

.remember-form textarea {
  font-weight: 400;
  width: 100%;
  border: 1px solid var(--line);
  border-radius: 10px;
  background: var(--surface);
  padding: 10px 12px;
  font-size: 12px;
  line-height: 1.8;
  color: var(--ink);
  resize: vertical;
  min-height: 110px;
  min-width: 0;
}

.remember-form .note {
  margin: 0;
  font-size: 10px;
}

.diagnostic-list {
  list-style: none;
  margin: 0;
  padding: 0;
  border: 1px solid var(--line);
  border-radius: 14px;
  background: var(--surface);
  overflow: hidden;
}

.diagnostic-list li {
  display: grid;
  grid-template-columns: 72px minmax(0, 1.2fr) minmax(0, 1fr) minmax(0, 0.8fr);
  gap: 10px;
  align-items: center;
  padding: 14px 17px;
  border-bottom: 1px solid #edf0e7;
  font-size: 12px;
}

.diagnostic-list li:last-child {
  border-bottom: 0;
}

.diag-seq {
  font-variant-numeric: tabular-nums;
  color: #929c89;
  font-size: 11px;
}

.diag-status {
  color: var(--muted);
  overflow-wrap: anywhere;
}

.diagnostics-foot {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 14px;
  flex-wrap: wrap;
}

.diagnostics-foot p {
  margin: 0;
  font-size: 11px;
  max-width: 520px;
}

.diagnostics-foot .btn.danger {
  border-color: var(--danger-soft);
  background: var(--danger-soft);
}

/* ---------- 确认与对话框 ---------- */

.inline-confirm {
  position: fixed;
  left: 50%;
  bottom: 24px;
  transform: translateX(calc(-50% + 102px));
  z-index: 30;
  max-width: min(460px, calc(100vw - 36px));
  background: var(--surface);
  border: 1px solid var(--line);
  border-radius: 14px;
  box-shadow: 0 10px 34px rgba(43, 62, 43, 0.16);
  padding: 16px 18px;
  font-size: 12px;
}

.inline-confirm p {
  margin: 0 0 10px;
  line-height: 1.7;
}

.inline-confirm-actions {
  display: flex;
  justify-content: flex-end;
  gap: 8px;
}

.inline-confirm-actions .btn {
  min-height: 32px;
  padding: 5px 12px;
  font-size: 12px;
}

.dialog-backdrop {
  position: fixed;
  inset: 0;
  background: rgba(38, 58, 43, 0.27);
  backdrop-filter: blur(3px);
  z-index: 40;
  display: flex;
  align-items: center;
  justify-content: center;
  padding: 20px;
}

.dialog {
  background: var(--paper);
  border: 1px solid #fff;
  border-radius: 20px;
  padding: 29px;
  max-width: 440px;
  width: 100%;
  box-shadow: 0 20px 70px rgba(28, 56, 34, 0.17);
}

.dialog h2 {
  margin: 0;
  font-size: 17px;
}

.dialog p {
  margin: 11px 0 0;
  font-size: 12px;
  color: var(--muted);
  line-height: 1.8;
}

.dialog-actions {
  display: flex;
  justify-content: flex-end;
  gap: 9px;
  margin-top: 22px;
}

.dialog-actions .btn {
  min-height: 36px;
}

/* ---------- 窄窗 ---------- */

@media (max-width: 1100px) {
  .app-shell {
    grid-template-columns: 180px minmax(0, 1fr);
  }

  .sidebar {
    padding: 23px 12px 13px;
  }

  .topbar,
  .page-heading {
    padding-left: 24px;
    padding-right: 24px;
  }

  .page-content {
    padding: 0 24px 24px;
  }

  .recovery-banner,
  .chat-warning,
  .chat-failure,
  .error-banner,
  .archived-banner {
    padding-left: 24px;
    padding-right: 24px;
  }

  .chat-body {
    grid-template-columns: 175px minmax(0, 1fr);
  }

  .sessions {
    padding-left: 10px;
    padding-right: 10px;
  }

  .conversation-head {
    padding: 14px 20px;
  }

  .messages {
    padding: 22px 21px 12px;
  }

  .composer-wrap {
    padding: 0 20px 16px;
  }

  .memory-layout {
    grid-template-columns: minmax(0, 1fr) 215px;
    gap: 16px;
  }

  .offline-note {
    display: none;
  }
}

@media (max-width: 800px) {
  .app-shell {
    grid-template-columns: 72px minmax(0, 1fr);
  }

  .sidebar {
    padding: 20px 10px 12px;
  }

  .brand {
    justify-content: center;
    margin: 0 0 28px;
  }

  .brand-name,
  .nav-label,
  .nav-item .nav-text,
  .nav-arrow,
  .companion-mini .grow,
  .companion-mini .dot,
  .sidebar-revoke span,
  .sidebar-tools span {
    display: none;
  }

  .nav-item {
    justify-content: center;
    padding: 13px 0;
  }

  .nav-item .icon {
    width: 21px;
    height: 21px;
  }

  .companion-mini {
    justify-content: center;
    padding: 14px 0;
  }

  .sidebar-tools {
    flex-direction: column;
    align-items: center;
  }

  .sidebar-revoke {
    justify-content: center;
  }

  .topbar,
  .page-heading {
    padding-left: 20px;
    padding-right: 20px;
  }

  .page-heading h1 {
    font-size: 21px;
  }

  .page-content {
    padding: 0 20px 20px;
  }

  .recovery-banner,
  .chat-warning,
  .chat-failure,
  .error-banner,
  .archived-banner {
    padding-left: 20px;
    padding-right: 20px;
  }

  .chat-body {
    grid-template-columns: 155px minmax(0, 1fr);
  }

  .session-chip .icon {
    display: none;
  }

  .messages {
    padding: 20px 16px 12px;
  }

  .composer-wrap {
    padding: 0 16px 14px;
  }

  .appearance-grid {
    grid-template-columns: 1fr;
  }

  .pet-preview {
    min-height: 240px;
  }

  .pet-drawing {
    width: 135px;
    height: 135px;
  }

  .memory-layout {
    grid-template-columns: 1fr;
  }

  .remember-form {
    position: static;
    order: -1;
  }

  .buddy-grid {
    grid-template-columns: 1fr;
  }

  .buddy-hero {
    padding: 22px;
  }

  .buddy-hero h2 {
    font-size: 21px;
  }

  .hero-cat {
    width: 105px;
    height: 105px;
  }

  .inline-confirm {
    transform: translateX(-50%);
  }
}

@media (max-width: 560px) {
  .app-shell {
    grid-template-columns: 56px minmax(0, 1fr);
  }

  .sidebar {
    padding-left: 7px;
    padding-right: 7px;
  }

  .topbar {
    padding: 0 14px;
    gap: 5px;
  }

  .crumb-root,
  .slash {
    display: none;
  }

  .page-heading {
    padding: 18px 16px;
    align-items: flex-start;
    flex-wrap: wrap;
  }

  .page-heading h1 {
    font-size: 19px;
  }

  .page-content {
    padding: 0 16px 18px;
  }

  .recovery-banner,
  .chat-warning,
  .chat-failure,
  .error-banner,
  .archived-banner {
    padding-left: 16px;
    padding-right: 16px;
  }

  .chat-body {
    grid-template-columns: 1fr;
    grid-template-rows: auto minmax(0, 1fr);
  }

  .sessions {
    flex-direction: row;
    gap: 6px;
    overflow: auto;
    max-height: 64px;
    border-right: 0;
    border-bottom: 1px solid var(--line);
    padding: 8px 12px;
    align-items: center;
  }

  .session-top {
    padding: 0;
  }

  .session-top strong,
  .session-bottom {
    display: none;
  }

  .session-scroll {
    display: flex;
    gap: 4px;
    overflow: visible;
    flex: initial;
  }

  .session-chip {
    white-space: nowrap;
    width: auto;
    margin: 0;
    padding: 8px 12px;
  }

  .session-sub {
    display: none;
  }

  .messages {
    padding: 18px 14px 10px;
  }

  .message-body {
    max-width: 88%;
  }

  .bubble {
    padding: 11px 13px;
    font-size: 12px;
  }

  .buddy-hero {
    padding: 19px;
  }

  .hero-cat {
    width: 78px;
    height: 78px;
  }

  .diagnostic-list li {
    grid-template-columns: 56px minmax(0, 1fr);
  }

  .diag-status {
    grid-column: 2;
  }

  .inline-confirm {
    transform: translateX(-50%);
    left: 50%;
  }
}
</style>
