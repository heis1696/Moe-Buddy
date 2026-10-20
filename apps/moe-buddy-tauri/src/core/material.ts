/**
 * 阶段4/C 冻结的素材呈现协议。
 *
 * 这里只做严格解析、命令构造与修订排序；文件读取、解码、播放器与权限判定都
 * 留在宿主。素材不是档案权威、模型输入或授权入口，前端不得从素材状态推导伙伴
 * 身份、私密数据或业务权限。
 */

import { IPC_CONTRACT } from './contract';
import type { BuddyExpectation, MaterialFrameTriple, PrototypeError } from './types';

export type MaterialSource = 'builtin' | 'package' | 'empty';
export type MaterialStatus = 'playing' | 'paused' | 'stopped' | 'faulted';

export interface MaterialPackView {
  id: string;
  label: string;
  version: number;
}

export interface MaterialActionView {
  id: string;
  label: string;
}

/** 素材状态里的失败只暴露固定安全码与文案，不含路径、票据或宿主原始错误正文。 */
export interface MaterialErrorView {
  code: string;
  message: string;
}

/**
 * `prototype_material_state` / `prototype://material` 的同一只读投影。
 * 字段严格冻结；不含路径、伙伴信息、票据或业务正文。
 *
 * `source` 描述当前已装资源，而不是播放状态：停止播放后仍可为 `package` 并保留
 * `decodedBytes`/`actions`，只有卸载才会成为 `empty` 且资源计数清零。帧快照在
 * 停止时另行发布显式 empty，不能用第 0 帧冒充仍在播放。
 */
export interface MaterialState {
  revision: number;
  resourceGeneration: number;
  playbackGeneration: number;
  source: MaterialSource;
  pack: MaterialPackView | null;
  actions: MaterialActionView[];
  actionId: string | null;
  status: MaterialStatus;
  loading: boolean;
  decodedBytes: number;
  frameCount: number;
  error: MaterialErrorView | null;
}

/**
 * `prototype://frame` / `prototype_frame` 的严格必填描述。正式包帧必须通过
 * `prototype_material_pixels` 按三元组取当前帧，本描述不携带像素或路径。
 */
export interface MaterialFrameDescriptor {
  resourceGeneration: number;
  playbackGeneration: number;
  presentationSequence: number;
  source: MaterialSource;
  clipId: string | null;
  frameIndex: number | null;
  width: number;
  height: number;
  paused: boolean;
  faulted: boolean;
}

/** `prototype_material_command` 的 `command` 使用 `type` 判别。 */
export type MaterialCommandType = 'load' | 'play' | 'pause' | 'resume' | 'stop' | 'unload' | 'builtin';

export interface MaterialGenerationExpectation {
  expectedResourceGeneration: number;
  expectedPlaybackGeneration: number;
}

export type MaterialCommand =
  | ({ type: 'load'; path: string } & MaterialGenerationExpectation)
  | ({ type: 'play'; actionId: string } & MaterialGenerationExpectation)
  | ({ type: 'pause' } & MaterialGenerationExpectation)
  | ({ type: 'resume' } & MaterialGenerationExpectation)
  | ({ type: 'stop' } & MaterialGenerationExpectation)
  | ({ type: 'unload' } & MaterialGenerationExpectation)
  | ({ type: 'builtin' } & MaterialGenerationExpectation);

export interface MaterialCommandEnvelope {
  command: MaterialCommand;
  context: { expectedBuddyId: string; expectedBuddiesRevision: number };
}

/** `prototype_material_command` 的回复：状态始终存在，error 为 null 或既有安全失败形状。 */
export interface MaterialReply {
  ok: boolean;
  state: MaterialState;
  error: PrototypeError | null;
}

/**
 * 宿主固定的呈现故障码：旧命中区域未确认清除时，MaterialHost 保持该故障投影并拒绝
 * 非恢复命令。前端只按固定码识别故障锁，不根据错误文案、状态文本或伙伴信息猜测。
 */
export const MATERIAL_HIT_MASK_CLEAR_FAILED = 'material_hit_mask_clear_failed';

/**
 * 故障锁是否生效。锁定时只能停止、卸载和只读刷新；只有收到更高 revision 且不再带
 * 该固定码的实际恢复投影（revision 排序由 `RevisionOrderedMaterialStates` 保证）
 * 才能恢复正常入口。
 */
export function materialRecoveryLocked(state: MaterialState | null): boolean {
  return state !== null && state.error?.code === MATERIAL_HIT_MASK_CLEAR_FAILED;
}

export const MATERIAL_SOURCES: readonly MaterialSource[] = ['builtin', 'package', 'empty'];
export const MATERIAL_STATUSES: readonly MaterialStatus[] = ['playing', 'paused', 'stopped', 'faulted'];
export const MATERIAL_COMMAND_TYPES: readonly MaterialCommandType[] = [
  'load',
  'play',
  'pause',
  'resume',
  'stop',
  'unload',
  'builtin',
];

/** 与共享夹具保持同一数值，避免前后端各写一份预算。 */
export interface MaterialLimits {
  idMaxBytes: number;
  labelMaxScalars: number;
  packMaxClips: number;
  packMaxActions: number;
  clipMaxFrames: number;
  packageMaxFrames: number;
  frameMaxDimension: number;
  decodedMaxBytes: number;
  binaryMaxBytes: number;
  presentationBudgetBytes: number;
  displayMaxDimension: number;
  maskMaxDimension: number;
}

export const MATERIAL_LIMITS: MaterialLimits = IPC_CONTRACT.materialLimits as MaterialLimits;

/**
 * 单个素材包的完整 RGBA 解码预算：共享投影额度同时覆盖旧包与唯一候选，因此单包是
 * 其中一半（32MiB）。面板只展示单包可用容量，不把合计量误报成用户素材额度。
 */
export const MATERIAL_PACK_PIXEL_BUDGET_BYTES = MATERIAL_LIMITS.decodedMaxBytes / 2;

const ID_PATTERN = /^[A-Za-z0-9_-]+$/;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function hasExactKeys(value: Record<string, unknown>, keys: readonly string[]): boolean {
  const actual = Object.keys(value).sort();
  const expected = [...keys].sort();
  return actual.length === expected.length && actual.every((key, index) => key === expected[index]);
}

function isPositiveSafeInteger(value: unknown): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value > 0;
}

function isSafeCount(value: unknown, maximum: number): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 && value <= maximum;
}

/** 素材 ID：1..64 字节 ASCII 字母/数字/下划线/连字符。 */
export function isMaterialId(value: unknown): value is string {
  return (
    typeof value === 'string' &&
    value.length >= 1 &&
    value.length <= MATERIAL_LIMITS.idMaxBytes &&
    ID_PATTERN.test(value)
  );
}

function isUnicodeScalarText(value: string, maximum: number): boolean {
  if (value.length < 1) return false;
  let scalars = 0;
  for (const character of value) {
    const point = character.codePointAt(0);
    if (point === undefined) return false;
    // 控制字符与孤立代理项都不是冻结契约允许的显示文本。
    if (point <= 0x1f || (point >= 0x7f && point <= 0x9f)) return false;
    if (point >= 0xd800 && point <= 0xdfff) return false;
    scalars += 1;
    if (scalars > maximum) return false;
  }
  return true;
}

/** 素材显示名：1..64 个非控制 Unicode 标量。 */
export function isMaterialLabel(value: unknown): value is string {
  return typeof value === 'string' && isUnicodeScalarText(value, MATERIAL_LIMITS.labelMaxScalars);
}

function parsePack(value: unknown): MaterialPackView | null | undefined {
  if (value === null) return null;
  if (!isRecord(value) || !hasExactKeys(value, IPC_CONTRACT.materialPackFields)) return undefined;
  if (!isMaterialId(value.id) || !isMaterialLabel(value.label) || !isPositiveSafeInteger(value.version)) {
    return undefined;
  }
  return { id: value.id, label: value.label, version: value.version };
}

function parseActions(value: unknown): MaterialActionView[] | null {
  if (!Array.isArray(value) || value.length > MATERIAL_LIMITS.packMaxActions) return null;
  const actions: MaterialActionView[] = [];
  const seen = new Set<string>();
  for (const item of value) {
    if (!isRecord(item) || !hasExactKeys(item, IPC_CONTRACT.materialActionFields)) return null;
    if (!isMaterialId(item.id) || !isMaterialLabel(item.label) || seen.has(item.id)) return null;
    seen.add(item.id);
    actions.push({ id: item.id, label: item.label });
  }
  return actions;
}

function parseMaterialError(value: unknown): MaterialErrorView | null | undefined {
  if (value === null) return null;
  if (!isRecord(value) || !hasExactKeys(value, IPC_CONTRACT.materialErrorFields)) return undefined;
  if (typeof value.code !== 'string' || typeof value.message !== 'string') return undefined;
  // 固定安全契约只约束有界文本；不再凭斜杠猜测是否含路径，也不改写宿主回执。
  if (!isUnicodeScalarText(value.code, 128) || !isUnicodeScalarText(value.message, 512)) {
    return undefined;
  }
  return { code: value.code, message: value.message };
}

/** 严格解析素材状态；缺字段、多字段、旧协议或矛盾投影一律拒绝。 */
export function parseMaterialState(value: unknown): MaterialState | null {
  if (!isRecord(value) || !hasExactKeys(value, IPC_CONTRACT.materialStateFields)) return null;
  if (
    !isPositiveSafeInteger(value.revision) ||
    !isPositiveSafeInteger(value.resourceGeneration) ||
    !isPositiveSafeInteger(value.playbackGeneration)
  ) {
    return null;
  }
  if (!MATERIAL_SOURCES.includes(value.source as MaterialSource)) return null;
  if (!MATERIAL_STATUSES.includes(value.status as MaterialStatus)) return null;
  if (typeof value.loading !== 'boolean') return null;
  if (!isSafeCount(value.decodedBytes, MATERIAL_LIMITS.decodedMaxBytes)) return null;
  if (!isSafeCount(value.frameCount, MATERIAL_LIMITS.packageMaxFrames)) return null;

  const pack = parsePack(value.pack);
  if (pack === undefined) return null;
  const actions = parseActions(value.actions);
  if (actions === null) return null;
  const actionId = value.actionId;
  if (actionId !== null) {
    if (!isMaterialId(actionId)) return null;
    if (!actions.some((action) => action.id === actionId)) return null;
  }
  const error = parseMaterialError(value.error);
  if (error === undefined) return null;

  if (value.source === 'package') {
    if (pack === null) return null;
  } else if (pack !== null) {
    return null;
  }
  if (value.source === 'empty') {
    // empty 表示未装任何资源：动作目录、字节与帧计数都必须归零。
    if (
      actionId !== null ||
      actions.length !== 0 ||
      value.frameCount !== 0 ||
      value.decodedBytes !== 0
    ) {
      return null;
    }
  }

  return {
    revision: value.revision,
    resourceGeneration: value.resourceGeneration,
    playbackGeneration: value.playbackGeneration,
    source: value.source as MaterialSource,
    pack,
    actions,
    actionId,
    status: value.status as MaterialStatus,
    loading: value.loading,
    decodedBytes: value.decodedBytes,
    frameCount: value.frameCount,
    error,
  };
}

/**
 * 严格解析宿主帧描述。builtin 固定 `idle-four-frame` 320×320；package 使用真实
 * Clip 与 0..255 帧索引；empty 必须显式 null/0，不允许用第 0 帧冒充仍在播放。
 */
export function parseMaterialFrame(value: unknown): MaterialFrameDescriptor | null {
  if (!isRecord(value) || !hasExactKeys(value, IPC_CONTRACT.frameFields)) return null;
  if (
    !isPositiveSafeInteger(value.resourceGeneration) ||
    !isPositiveSafeInteger(value.playbackGeneration) ||
    !isPositiveSafeInteger(value.presentationSequence)
  ) {
    return null;
  }
  if (!MATERIAL_SOURCES.includes(value.source as MaterialSource)) return null;
  if (typeof value.paused !== 'boolean' || typeof value.faulted !== 'boolean') return null;

  if (value.source === 'empty') {
    if (value.clipId !== null || value.frameIndex !== null) return null;
    if (value.width !== 0 || value.height !== 0) return null;
  } else if (value.source === 'builtin') {
    if (
      value.clipId !== 'idle-four-frame' ||
      value.frameIndex !== 0 &&
        value.frameIndex !== 1 &&
        value.frameIndex !== 2 &&
        value.frameIndex !== 3
    ) {
      return null;
    }
    if (value.width !== 320 || value.height !== 320) return null;
  } else {
    if (!isMaterialId(value.clipId)) return null;
    if (!isSafeCount(value.frameIndex, 255)) return null;
    if (
      !isSafeCount(value.width, MATERIAL_LIMITS.frameMaxDimension) ||
      !isSafeCount(value.height, MATERIAL_LIMITS.frameMaxDimension) ||
      value.width < 1 ||
      value.height < 1
    ) {
      return null;
    }
  }

  return {
    resourceGeneration: value.resourceGeneration,
    playbackGeneration: value.playbackGeneration,
    presentationSequence: value.presentationSequence,
    source: value.source as MaterialSource,
    clipId: value.clipId as string | null,
    frameIndex: value.frameIndex as number | null,
    width: value.width as number,
    height: value.height as number,
    paused: value.paused,
    faulted: value.faulted,
  };
}

export function materialFrameTriple(frame: MaterialFrameDescriptor): MaterialFrameTriple {
  return {
    resourceGeneration: frame.resourceGeneration,
    playbackGeneration: frame.playbackGeneration,
    presentationSequence: frame.presentationSequence,
  };
}

export function sameMaterialTriple(left: MaterialFrameTriple | null, right: MaterialFrameTriple | null): boolean {
  if (left === null || right === null) return left === right;
  return (
    left.resourceGeneration === right.resourceGeneration &&
    left.playbackGeneration === right.playbackGeneration &&
    left.presentationSequence === right.presentationSequence
  );
}

/** 回复里的 error 沿用既有安全失败形状；前端只展示固定文案。 */
function parseSafeFailure(value: unknown): PrototypeError | null | undefined {
  if (value === null) return null;
  if (!isRecord(value) || !hasExactKeys(value, IPC_CONTRACT.errorFields)) return undefined;
  if (typeof value.code !== 'string' || typeof value.message !== 'string') return undefined;
  if (!IPC_CONTRACT.dispositions.includes(value.disposition as never)) return undefined;
  return {
    code: value.code,
    message: value.message,
    disposition: value.disposition as PrototypeError['disposition'],
  };
}

/** `prototype_material_command` 回复：`{ ok, state, error }` 三个字段全部必填。 */
export function parseMaterialReply(value: unknown): MaterialReply | null {
  if (!isRecord(value) || !hasExactKeys(value, IPC_CONTRACT.materialReplyFields)) return null;
  if (typeof value.ok !== 'boolean') return null;
  const state = parseMaterialState(value.state);
  if (state === null) return null;
  const error = parseSafeFailure(value.error);
  if (error === undefined) return null;
  if (value.ok !== (error === null)) return null;
  return { ok: value.ok, state, error };
}

/**
 * 导入路径只接受本地磁盘绝对路径（Windows 盘符）。UNC、设备/扩展前缀、相对
 * 路径、冒号与控制字符都在前端明确拒绝；最终根目录归属仍由宿主复验。
 */
export function isLocalMaterialPath(value: unknown): value is string {
  if (typeof value !== 'string') return false;
  if (value.length < 3 || value.length > 32767) return false;
  if (/[\u0000-\u001f\u007f]/.test(value)) return false;
  if (value.startsWith('\\\\') || value.startsWith('//')) return false;
  if (!/^[A-Za-z]:[\\/]/.test(value)) return false;
  if (value.slice(2).includes(':')) return false;
  const parts = value.slice(3).split(/[\\/]+/);
  if (parts.some((part) => part === '.' || part === '..')) return false;
  return true;
}

function generationFields(state: MaterialState): MaterialGenerationExpectation {
  if (!isPositiveSafeInteger(state.resourceGeneration) || !isPositiveSafeInteger(state.playbackGeneration)) {
    throw new TypeError('material state is missing valid generations');
  }
  return {
    expectedResourceGeneration: state.resourceGeneration,
    expectedPlaybackGeneration: state.playbackGeneration,
  };
}

export function loadMaterialCommand(path: string, state: MaterialState): MaterialCommand {
  if (!isLocalMaterialPath(path)) throw new TypeError('material load path must be an absolute local path');
  return { type: 'load', path, ...generationFields(state) };
}

export function playMaterialCommand(actionId: string, state: MaterialState): MaterialCommand {
  if (!isMaterialId(actionId)) throw new TypeError('material action id is invalid');
  return { type: 'play', actionId, ...generationFields(state) };
}

export function pauseMaterialCommand(state: MaterialState): MaterialCommand {
  return { type: 'pause', ...generationFields(state) };
}

export function resumeMaterialCommand(state: MaterialState): MaterialCommand {
  return { type: 'resume', ...generationFields(state) };
}

export function stopMaterialCommand(state: MaterialState): MaterialCommand {
  return { type: 'stop', ...generationFields(state) };
}

export function unloadMaterialCommand(state: MaterialState): MaterialCommand {
  return { type: 'unload', ...generationFields(state) };
}

export function builtinMaterialCommand(state: MaterialState): MaterialCommand {
  return { type: 'builtin', ...generationFields(state) };
}

export function materialCommandEnvelope(
  command: MaterialCommand,
  expectation: BuddyExpectation,
): MaterialCommandEnvelope {
  return {
    command,
    context: {
      expectedBuddyId: expectation.expectedBuddyId,
      expectedBuddiesRevision: expectation.expectedBuddiesRevision,
    },
  };
}

export interface MaterialLoadIntent {
  ok: boolean;
  path?: string;
  message?: string;
}

/** 一次拖入只接受一个目录/manifest.json；多路径或非法路径给出固定安全提示。 */
export function materialDropIntent(paths: unknown): MaterialLoadIntent {
  if (!Array.isArray(paths) || paths.length === 0) {
    return { ok: false, message: '未收到可导入的本地素材路径。' };
  }
  if (paths.length !== 1) {
    return { ok: false, message: '一次只能拖入一个素材包目录或其中的 manifest.json。' };
  }
  const path = paths[0];
  if (!isLocalMaterialPath(path)) {
    return { ok: false, message: '素材路径必须是本机磁盘绝对路径，不支持网络、设备或相对路径。' };
  }
  return { ok: true, path };
}

export interface MaterialDropPoint {
  x: number;
  y: number;
}

export interface MaterialDropRect {
  left: number;
  top: number;
  width: number;
  height: number;
}

function isFiniteNumber(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value);
}

/**
 * 原生拖放位置是窗口物理像素，元素矩形是 CSS 像素；按实际 DPR 换算后判断是否落在
 * 素材区域内。位置、矩形或 DPR 缺失/非法时一律视为区域外，不发起加载。
 */
export function materialDropWithinArea(
  point: unknown,
  rect: unknown,
  devicePixelRatio: unknown,
): boolean {
  if (!isRecord(point) || !isFiniteNumber(point.x) || !isFiniteNumber(point.y)) return false;
  if (!isRecord(rect)) return false;
  const { left, top, width, height } = rect;
  if (
    !isFiniteNumber(left) ||
    !isFiniteNumber(top) ||
    !isFiniteNumber(width) ||
    !isFiniteNumber(height) ||
    width <= 0 ||
    height <= 0
  ) {
    return false;
  }
  const dpr =
    typeof devicePixelRatio === 'number' &&
    Number.isFinite(devicePixelRatio) &&
    devicePixelRatio > 0
      ? devicePixelRatio
      : 1;
  const right = (left + width) * dpr;
  const bottom = (top + height) * dpr;
  return point.x >= left * dpr && point.x <= right && point.y >= top * dpr && point.y <= bottom;
}

/**
 * 素材状态按 `revision` 严格单调：初始查询、事件与命令回复共用同一入口，
 * 旧查询/事件不得覆盖新状态；同修订只接受一次。
 */
export class RevisionOrderedMaterialStates {
  private latest: MaterialState | null = null;

  get current(): MaterialState | null {
    return this.latest;
  }

  reset(): void {
    this.latest = null;
  }

  apply(candidate: MaterialState): boolean {
    if (this.latest !== null && candidate.revision <= this.latest.revision) return false;
    this.latest = candidate;
    return true;
  }
}

/** 供界面安全展示：素材状态只暴露固定错误文案，不回显路径或宿主原始错误。 */
export function materialStateErrorText(state: MaterialState | null): string | null {
  if (state === null || state.error === null) return null;
  return state.error.message;
}
