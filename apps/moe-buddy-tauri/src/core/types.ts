/** 冻结的 Tauri 原型 IPC 数据形状。前端只消费这些只读快照，不持有业务状态。 */

export interface ChatMessage {
  role: string;
  text: string;
}

export interface PrototypeError {
  code: string;
  message: string;
  disposition: 'not_executed' | 'settled' | 'unknown';
}

/** 宿主 `ChatSnapshot.failure` 是可选的安全失败码字符串，不是任意结构。 */
export type SnapshotFailure = string | null;

export interface ChatSnapshot {
  draft: string;
  messages: ChatMessage[];
  status: string;
  failure: SnapshotFailure;
}

export type PanelName = 'presets' | 'memory' | 'diagnostics';

export interface PresetView {
  id: string;
  label: string;
  provider: string;
  model: string;
  endpoint: string;
}

export interface MemoryView {
  id: string;
  revision: number;
  text: string;
}

export interface DiagnosticView {
  sequence: number;
  run: string;
  event: string;
  status: string;
}

/**
 * 非凭据模型配置。宿主承诺这里只有展示字段和 `hasApiKey` 布尔值，
 * 不含 API Key 正文；前端也不得把它写回快照或本地存储。
 */
export interface ModelSettingsView {
  endpoint: string;
  model: string;
  persona: string;
  allowTransmission: boolean;
  hasApiKey: boolean;
}

/** 受保护管理草稿的种类；`none` 只与 `dirty=false` 组合。 */
export type ManagementDraftKind = 'none' | 'general' | 'buddy_create' | 'buddy_rename';

export interface ManagementDraftView {
  version: number;
  dirty: boolean;
  kind: ManagementDraftKind;
}

export interface ManagementExitView {
  version: number;
  intent: 'close_chat' | 'quit_application';
}

/** 权威聊天草稿丢弃回执：只含会话 ID，永远不含草稿正文。 */
export interface ChatDraftDiscardView {
  /** 该次丢弃产生的草稿集合修订；后续编辑会让当前修订超过它。 */
  revision: number;
  /** 被该次明确丢弃清空的会话 ID，至多 64 项、去重且无正文。 */
  sessionIds: string[];
}

/**
 * 单个会话的草稿水位：最近一次草稿变更修订与最近一次明确丢弃修订。
 * `discardedRevision` 为 null 表示该会话尚未被明确丢弃。前端据此逐会话
 * 消费丢弃，不能因为其他会话编辑推进了全局修订就跳过本会话水位。
 */
export interface ChatDraftSessionView {
  id: string;
  revision: number;
  discardedRevision: number | null;
}

/** 应用持有的聊天草稿集合只读视图；不是客户端草稿缓存的业务权威。 */
export interface ChatDraftView {
  revision: number;
  discard: ChatDraftDiscardView | null;
  /** 按会话有界（至多 64 项）的丢弃水位；撤权后为空。 */
  sessions: ChatDraftSessionView[];
}

/**
 * B 伙伴档案：稳定实例 ID 由受信注册服务签发，前端只读展示；
 * 档案/人设/模型配置各自带版本，显示名与素材都不是身份。
 */
export interface BuddyView {
  id: string;
  label: string;
  profileRevision: number;
  personaRevision: number;
  modelRevision: number;
}

/** 宿主签发的只读伙伴注册表；列表不授予私密数据或外传权限。 */
export interface BuddyCatalogView {
  activeBuddyId: string | null;
  buddiesRevision: number;
  buddies: BuddyView[];
}

/** 已成功档案动作的种类；回执只带控制信息，不带正文、Key、路径或票据。 */
export type BuddyCommitKind =
  | 'create_buddy'
  | 'switch_buddy'
  | 'rename_buddy'
  | 'configure_model'
  | 'select_preset';

/**
 * 宿主在持久化及所需运行绑定实际完成后签发的最近一次成功档案回执。
 * `toRevision` 必须等于 `fromRevision + 1`；`managementVersion` 是受理时的
 * 管理状态版本。它不是执行票据，也不能解除其他 Unknown。
 */
export interface BuddyCommitReceipt {
  kind: BuddyCommitKind;
  fromBuddyId: string;
  fromRevision: number;
  toBuddyId: string;
  toRevision: number;
  managementVersion: number;
}

/**
 * 每个业务命令发起时捕获的伙伴上下文。宿主按它复验选择与注册表修订；
 * 空串 `expectedBuddyId` 表示“宿主尚未签发任何伙伴”，不是伪造的伙伴 ID。
 */
export interface BuddyExpectation {
  expectedBuddyId: string;
  expectedBuddiesRevision: number;
}

/** 会话目录条目沿用聊天业务状态；归档只限制发送，不删除状态或正文。 */
export type SessionStatus = 'ready' | 'sending' | 'failed' | 'unknown';

export interface SessionView {
  id: string;
  label: string;
  archived: boolean;
  status: SessionStatus;
}

/** 宿主签发的只读会话目录；前端只选择和展示，不提供归属或权限字段。 */
export interface SessionCatalogView {
  activeSessionId: string | null;
  sessionsRevision: number;
  sessions: SessionView[];
}

/**
 * 生命周期命令必须携带的当前预期。`expectedSessionId` 可为 null 表示宿主当前
 * 尚未签发会话；前端不会用它代替归属，也不会凭空构造会话 ID。
 */
export interface SessionExpectation {
  expectedSessionId: string | null;
  expectedSessionsRevision: number;
}

export interface PrototypeSnapshot {
  revision: number;
  presentationRevision: number;
  mode: string;
  revoked: boolean;
  chat: ChatSnapshot;
  closeConfirmation: boolean;
  closeIntent: 'close_chat' | 'quit_application' | null;
  managementDraft: ManagementDraftView;
  managementExit: ManagementExitView | null;
  managementDiscardedVersion: number | null;
  chatDraft: ChatDraftView;
  activePreset: string;
  externalTransmissionAllowed: boolean;
  presets: PresetView[];
  memories: MemoryView[];
  diagnostics: DiagnosticView[];
  notice: string | null;
  modelSettings: ModelSettingsView | null;
  persistenceEnabled: boolean;
  /**
   * B 伙伴注册表三元组同样是当前冻结协议的必填字段；它与 A 的三元组一起
   * 让缺字段的旧快照整份拒绝，前端不维护临时双版本兼容。
   */
  activeBuddyId: string | null;
  buddiesRevision: number;
  buddies: BuddyView[];
  /**
   * 当前受信档案的人设。未配置模型的伙伴仍需要它来显示和初始化配置表单；
   * 不返回其他伙伴正文。无活动伙伴或撤权时为 null。
   */
  activeBuddyPersona: string | null;
  /**
   * 最近一次已成功档案动作的无正文回执；启动、撤权或无档案动作为 null。
   * 前端只按发起时捕获的完整 context、动作、管理版本与本地编辑代际匹配消费。
   */
  buddyCommit: BuddyCommitReceipt | null;
  /**
   * A 会话目录三元组与隔离许可都是当前冻结协议的必填字段。
   * 缺字段的旧快照整份拒绝，不再维护临时双版本兼容。
   */
  activeSessionId: string | null;
  sessionsRevision: number;
  sessions: SessionView[];
  /**
   * 由受信应用裁决：当前会话确为外部结果未知、本地处理已结束、且没有
   * 存储隔离/撤权/记忆配置等全局未知时才为 true。前端只消费该布尔值，
   * 不得从 `send` 操作名或错误码推断。
   */
  sessionIsolationAllowed: boolean;
}

/** 冻结 envelope：snapshot 与 error 两个字段必须同时存在。 */
export interface SnapshotResponse {
  snapshot: PrototypeSnapshot | null;
  error: PrototypeError | null;
}

/**
 * 宿主帧世代三元组。资源代际在装包、卸载或恢复 builtin 时推进；播放代际在
 * 实际新播放/停止时推进；展示序号在每次帧、暂停、故障或空状态变化时单调推进。
 * 前端只能回传宿主已发布的当前三元组，不能自行续号。
 */
export interface MaterialFrameTriple {
  resourceGeneration: number;
  playbackGeneration: number;
  presentationSequence: number;
}

/**
 * 命中掩码必须携带产生它的帧三元组；宿主在实际原生应用掩码时复验当前三元组，
 * 失配即拒绝，因此停止/切包后的迟到掩码不会覆盖新画面。
 */
export interface HitMaskPayload {
  width: number;
  height: number;
  alpha: number[];
  frame: MaterialFrameTriple;
}

export interface ManagementDraftAck {
  version: number;
  dirty: boolean;
}
