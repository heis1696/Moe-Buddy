import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { PrototypeBoundaryError } from '../core/failure';
import { parseHitMaskError } from '../core/hitMask';
import {
  parseMaterialFrame,
  parseMaterialReply,
  parseMaterialState,
  type MaterialCommand,
  type MaterialFrameDescriptor,
  type MaterialReply,
  type MaterialState,
} from '../core/material';
import {
  IPC,
  dispatchContext,
  dispatchEnvelope,
  dispatchSourceAllowed,
  windowCommandAllowed,
  windowCommandPayload,
  type DispatchCommand,
  type WindowCommand,
} from '../core/protocol';
import { parseSnapshotResponse } from '../core/snapshot';
import type {
  BuddyExpectation,
  HitMaskPayload,
  ManagementDraftAck,
  ManagementDraftKind,
  MaterialFrameTriple,
  SnapshotResponse,
} from '../core/types';

/**
 * 原生拖放信号保留路径与物理坐标。坐标原样转交界面做素材区域判断；不写入日志、
 * 状态或响应，也不把路径回显给用户。
 */
export interface DragDropSignal {
  type: 'enter' | 'over' | 'drop' | 'leave';
  paths: string[];
  position: { x: number; y: number } | null;
}

/** 普通浏览器没有 Tauri 注入；该判断只用于提示从桌面启动，不启用任何模拟数据。 */
export function runningInTauri(): boolean {
  if (isTauri()) return true;
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

export function currentWindowLabel(): string | null {
  if (!runningInTauri()) return null;
  try {
    return getCurrentWindow().label;
  } catch {
    return null;
  }
}

async function invokeFixed<T>(
  command: string,
  args: Record<string, unknown> | undefined,
  code: string,
  message: string,
): Promise<T> {
  try {
    return await invoke<T>(command, args);
  } catch {
    // 原始 invoke 错误不进入界面；只暴露本适配层定义的固定安全错误。
    throw new PrototypeBoundaryError(code, message);
  }
}

function parseSnapshotOrThrow(payload: unknown): SnapshotResponse {
  const parsed = parseSnapshotResponse(payload);
  if (parsed === null) {
    throw new PrototypeBoundaryError('invalid_snapshot', '桌面宿主返回了无法识别的聊天状态');
  }
  return parsed;
}

export async function requestSnapshot(): Promise<SnapshotResponse> {
  assertDispatchSource();
  const payload = await invokeFixed<unknown>(
    IPC.snapshot,
    undefined,
    'snapshot_unavailable',
    '无法从桌面宿主读取聊天状态',
  );
  return parseSnapshotOrThrow(payload);
}

/**
 * 受控派发：`{ command, context }` 是冻结负载。`context` 必须由调用方在发起
 * 命令时捕获，本适配层不使用“当前伙伴”临时替换，也不提供省略上下文的重载。
 */
export async function dispatchPrototype(
  command: DispatchCommand,
  expectation: BuddyExpectation,
): Promise<SnapshotResponse> {
  assertDispatchSource();
  const envelope = dispatchEnvelope(command, expectation);
  const payload = await invokeFixed<unknown>(
    IPC.dispatch,
    { command: envelope.command, context: envelope.context },
    'dispatch_unavailable',
    '桌面宿主未能完成聊天命令',
  );
  return parseSnapshotOrThrow(payload);
}

/** 管理编辑同步同样携带发起时捕获的伙伴上下文与必填种类；回执仍只有 `{ version, dirty }`。 */
export async function updateManagementDraftState(
  version: number,
  dirty: boolean,
  kind: ManagementDraftKind,
  expectation: BuddyExpectation,
): Promise<ManagementDraftAck> {
  assertDispatchSource();
  const payload = await invokeFixed<unknown>(
    IPC.managementDraftState,
    { version, dirty, kind, context: dispatchContext(expectation) },
    'management_state_unavailable',
    '管理编辑状态未能同步到桌面宿主',
  );
  if (
    typeof payload !== 'object' ||
    payload === null ||
    Array.isArray(payload) ||
    Object.keys(payload).sort().join(',') !== 'dirty,version' ||
    typeof (payload as Record<string, unknown>).version !== 'number' ||
    !Number.isSafeInteger((payload as Record<string, unknown>).version) ||
    typeof (payload as Record<string, unknown>).dirty !== 'boolean'
  ) {
    throw new PrototypeBoundaryError(
      'invalid_management_state',
      '桌面宿主返回了无法识别的管理编辑状态',
    );
  }
  return {
    version: (payload as Record<string, unknown>).version as number,
    dirty: (payload as Record<string, unknown>).dirty as boolean,
  };
}

/**
 * 窗口命令适配：主窗 `show_chat | hide_chat | quit` 必须由调用方传入发起时
 * 捕获的完整伙伴上下文；适配层只固化该值，绝不在异步前改读“当前伙伴”。
 * 宠物窗仅保留既有 show_chat 聚焦与呈现/拖动，不携带伙伴上下文。
 */
export async function invokeWindowCommand(
  command: WindowCommand,
  expectation?: BuddyExpectation,
): Promise<void> {
  const label = currentWindowLabel();
  if (label !== null && !windowCommandAllowed(label, command)) {
    throw new PrototypeBoundaryError(
      'window_command_not_allowed',
      '当前窗口不允许执行该窗口命令',
    );
  }
  const payload = windowCommandPayload(command, label, expectation);
  await invokeFixed<unknown>(
    IPC.window,
    { ...payload },
    'window_unavailable',
    '桌面宿主未能完成窗口命令',
  );
}

function assertDispatchSource(): void {
  const label = currentWindowLabel();
  if (label !== null && !dispatchSourceAllowed(label)) {
    throw new PrototypeBoundaryError(
      'dispatch_source_not_allowed',
      '聊天读取与派发仅允许主窗口调用',
    );
  }
}

export async function setHitMask(mask: HitMaskPayload): Promise<void> {
  const label = currentWindowLabel();
  if (label !== null && label !== 'pet') {
    throw new PrototypeBoundaryError('hit_mask_not_allowed', '命中区域只允许宠物窗写入');
  }
  await invokeFixed<unknown>(
    IPC.setHitMask,
    {
      width: mask.width,
      height: mask.height,
      alpha: mask.alpha,
      frame: {
        resourceGeneration: mask.frame.resourceGeneration,
        playbackGeneration: mask.frame.playbackGeneration,
        presentationSequence: mask.frame.presentationSequence,
      },
    },
    'hit_mask_unavailable',
    '桌面宿主未能安装宠物命中区域',
  );
}

export async function requestFrame(): Promise<MaterialFrameDescriptor> {
  const payload = await invokeFixed<unknown>(
    IPC.frame,
    undefined,
    'frame_unavailable',
    '桌面宿主未能提供动画帧状态',
  );
  const parsed = parseMaterialFrame(payload);
  if (parsed === null) {
    throw new PrototypeBoundaryError('invalid_frame', '桌面宿主返回了无法识别的帧状态');
  }
  return parsed;
}

/** main/pet 都可读的纯呈现查询；不含路径、伙伴信息、票据或业务正文。 */
export async function requestMaterialState(): Promise<MaterialState> {
  const payload = await invokeFixed<unknown>(
    IPC.materialState,
    undefined,
    'material_state_unavailable',
    '桌面宿主未能提供素材状态',
  );
  const parsed = parseMaterialState(payload);
  if (parsed === null) {
    throw new PrototypeBoundaryError('invalid_material_state', '桌面宿主返回了无法识别的素材状态');
  }
  return parsed;
}

/**
 * 素材命令只允许主窗；发起时捕获的完整伙伴 context 原样携带，不用“当前伙伴”
 * 替换。回复只解析为冻结的 `{ ok, state, error }`。
 */
export async function dispatchMaterialCommand(
  command: MaterialCommand,
  expectation: BuddyExpectation,
): Promise<MaterialReply> {
  const label = currentWindowLabel();
  if (label !== null && label !== 'main') {
    throw new PrototypeBoundaryError(
      'material_command_not_allowed',
      '素材命令只允许主窗口调用',
    );
  }
  const envelope = {
    command,
    context: dispatchContext(expectation),
  };
  const payload = await invokeFixed<unknown>(
    IPC.materialCommand,
    { command: envelope.command, context: envelope.context },
    'material_command_unavailable',
    '桌面宿主未能处理素材命令',
  );
  const parsed = parseMaterialReply(payload);
  if (parsed === null) {
    throw new PrototypeBoundaryError('invalid_material_reply', '桌面宿主返回了无法识别的素材回执');
  }
  return parsed;
}

/** 只允许 pet 按当前三元组取帧；不接路径或 Clip 索引，不给任意资源读取能力。 */
export async function requestMaterialPixels(frame: MaterialFrameTriple): Promise<ArrayBuffer> {
  const label = currentWindowLabel();
  if (label !== null && label !== 'pet') {
    throw new PrototypeBoundaryError('material_pixels_not_allowed', '像素读取只允许宠物窗调用');
  }
  if (
    !Number.isSafeInteger(frame.resourceGeneration) ||
    frame.resourceGeneration < 1 ||
    !Number.isSafeInteger(frame.playbackGeneration) ||
    frame.playbackGeneration < 1 ||
    !Number.isSafeInteger(frame.presentationSequence) ||
    frame.presentationSequence < 1
  ) {
    throw new PrototypeBoundaryError('invalid_material_frame', '素材帧三元组无效');
  }
  const payload = await invokeFixed<unknown>(
    IPC.materialPixels,
    {
      frame: {
        resourceGeneration: frame.resourceGeneration,
        playbackGeneration: frame.playbackGeneration,
        presentationSequence: frame.presentationSequence,
      },
    },
    'material_pixels_unavailable',
    '桌面宿主未能提供当前素材帧',
  );
  if (payload instanceof ArrayBuffer) return payload;
  if (ArrayBuffer.isView(payload)) {
    const view = payload as ArrayBufferView;
    const copy = new Uint8Array(view.byteLength);
    copy.set(new Uint8Array(view.buffer, view.byteOffset, view.byteLength));
    return copy.buffer;
  }
  throw new PrototypeBoundaryError('invalid_material_pixels', '桌面宿主返回了无法识别的素材像素');
}

/** 素材状态事件只向 main/pet 发布同一投影；非法负载明确丢弃。 */
export function listenMaterialEvents(
  handler: (state: MaterialState | null) => void,
): Promise<UnlistenFn> {
  return listen<unknown>(IPC.materialEvent, (event) => {
    handler(parseMaterialState(event.payload));
  });
}

/** 复用 Tauri 已有原生拖放，不引入对话框插件或浏览器文件输入。 */
export function listenDragDropEvent(
  handler: (signal: DragDropSignal) => void,
): Promise<UnlistenFn> {
  return getCurrentWindow().onDragDropEvent((event) => {
    const payload = event.payload;
    if (payload.type === 'drop' || payload.type === 'enter') {
      handler({
        type: payload.type,
        paths: [...payload.paths],
        position: { x: payload.position.x, y: payload.position.y },
      });
      return;
    }
    if (payload.type === 'over') {
      handler({
        type: 'over',
        paths: [],
        position: { x: payload.position.x, y: payload.position.y },
      });
      return;
    }
    handler({ type: 'leave', paths: [], position: null });
  });
}

/** 直接返回宿主授予的 unlisten；释放结果由调用方通过 core/listeners 显式观察。 */
export function listenSnapshotEvents(
  handler: (response: SnapshotResponse | null) => void,
): Promise<UnlistenFn> {
  return listen<unknown>(IPC.snapshotEvent, (event) => {
    handler(parseSnapshotResponse(event.payload));
  });
}

export function listenFrameEvents(
  handler: (state: MaterialFrameDescriptor | null) => void,
): Promise<UnlistenFn> {
  return listen<unknown>(IPC.frameEvent, (event) => {
    handler(parseMaterialFrame(event.payload));
  });
}

/** 宿主命中/穿透系统调用失败时只发送固定错误码，界面统一映射为固定安全文案。 */
export function listenHitMaskErrorEvents(
  handler: (message: string | null) => void,
): Promise<UnlistenFn> {
  return listen<unknown>(IPC.hitMaskErrorEvent, (event) => {
    handler(parseHitMaskError(event.payload));
  });
}
