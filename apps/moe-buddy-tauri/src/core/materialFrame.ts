/**
 * 宠物窗的单帧呈现预算与有界取帧管线。
 *
 * 前端只保留一张源 RGBA、一份在途二进制及必要的当前/待替换 Canvas/ImageData；
 * 不缓存整包或每帧画布，也不使用第二套时间推进。宿主帧描述的三元组是唯一的
 * 回填凭据，迟到、重播、卸载与组件释放都不得复活旧像素。
 */

import type { CatFrameRaster } from './catFrame';
import { MATERIAL_LIMITS, sameMaterialTriple, type MaterialFrameDescriptor } from './material';
import type { MaterialFrameTriple } from './types';

export const MATERIAL_DISPLAY_MAX_DIMENSION = MATERIAL_LIMITS.displayMaxDimension;
export const MATERIAL_BINARY_MAX_BYTES = MATERIAL_LIMITS.binaryMaxBytes;
export const MATERIAL_FRAME_MAX_DIMENSION = MATERIAL_LIMITS.frameMaxDimension;
export const MATERIAL_PRESENTATION_BUDGET_BYTES = MATERIAL_LIMITS.presentationBudgetBytes;

export interface RgbaFrame {
  width: number;
  height: number;
  data: Uint8ClampedArray<ArrayBuffer>;
}

export interface DisplaySize {
  width: number;
  height: number;
}

export interface FramePlacement extends DisplaySize {
  offsetX: number;
  offsetY: number;
}

function isSafeDimension(value: unknown, maximum: number): value is number {
  return typeof value === 'number' && Number.isSafeInteger(value) && value >= 1 && value <= maximum;
}

/**
 * 把窗口物理像素等比压到宽高各至多 1024 的显示 backing；实际应用可控呈现副本
 * 仍受 16MiB 预算约束。非法窗口尺寸明确报错，不静默生成 0 尺寸画布。
 */
export function displayBackingSize(
  windowWidth: unknown,
  windowHeight: unknown,
  devicePixelRatio: unknown,
): DisplaySize {
  if (
    typeof windowWidth !== 'number' ||
    !Number.isFinite(windowWidth) ||
    windowWidth <= 0 ||
    typeof windowHeight !== 'number' ||
    !Number.isFinite(windowHeight) ||
    windowHeight <= 0
  ) {
    throw new RangeError('window size must be positive finite numbers');
  }
  const dpr =
    typeof devicePixelRatio === 'number' && Number.isFinite(devicePixelRatio) && devicePixelRatio >= 1
      ? devicePixelRatio
      : 1;
  const rawWidth = Math.max(1, Math.round(windowWidth * dpr));
  const rawHeight = Math.max(1, Math.round(windowHeight * dpr));
  const scale = Math.min(
    1,
    MATERIAL_DISPLAY_MAX_DIMENSION / rawWidth,
    MATERIAL_DISPLAY_MAX_DIMENSION / rawHeight,
  );
  const width = Math.max(1, Math.min(MATERIAL_DISPLAY_MAX_DIMENSION, Math.round(rawWidth * scale)));
  const height = Math.max(1, Math.min(MATERIAL_DISPLAY_MAX_DIMENSION, Math.round(rawHeight * scale)));
  // 1024×1024 RGBA 已远小于 16MiB；这里仍显式校验，防止未来放宽上限时静默越界。
  if (width * height * 4 > MATERIAL_PRESENTATION_BUDGET_BYTES) {
    throw new RangeError('display backing exceeds the presentation budget');
  }
  return { width, height };
}

/** 正式包帧在显示 backing 内等比 contain，居中；透明留边不会被误算成命中区域。 */
export function framePlacement(
  frameWidth: unknown,
  frameHeight: unknown,
  displayWidth: unknown,
  displayHeight: unknown,
): FramePlacement {
  if (
    !isSafeDimension(frameWidth, MATERIAL_FRAME_MAX_DIMENSION) ||
    !isSafeDimension(frameHeight, MATERIAL_FRAME_MAX_DIMENSION) ||
    !isSafeDimension(displayWidth, MATERIAL_DISPLAY_MAX_DIMENSION) ||
    !isSafeDimension(displayHeight, MATERIAL_DISPLAY_MAX_DIMENSION)
  ) {
    throw new RangeError('frame or display dimensions are outside the bounded range');
  }
  const scale = Math.min(displayWidth / frameWidth, displayHeight / frameHeight);
  const width = Math.max(1, Math.min(displayWidth, Math.round(frameWidth * scale)));
  const height = Math.max(1, Math.min(displayHeight, Math.round(frameHeight * scale)));
  return {
    offsetX: Math.floor((displayWidth - width) / 2),
    offsetY: Math.floor((displayHeight - height) / 2),
    width,
    height,
  };
}

/**
 * 校验并包装宿主返回的当前帧 RGBA。长度必须恰为 width×height×4，单帧二进制
 * 至多 1MiB；不做补边、截断或按比例猜测。返回的缓冲只作为一份源像素使用。
 */
export function rgbaFrameFromBinary(
  payload: unknown,
  width: unknown,
  height: unknown,
): RgbaFrame | null {
  if (!isSafeDimension(width, MATERIAL_FRAME_MAX_DIMENSION) || !isSafeDimension(height, MATERIAL_FRAME_MAX_DIMENSION)) {
    return null;
  }
  const expected = width * height * 4;
  if (!Number.isSafeInteger(expected) || expected > MATERIAL_BINARY_MAX_BYTES) return null;

  let buffer: ArrayBuffer;
  if (payload instanceof ArrayBuffer) {
    buffer = payload;
  } else if (ArrayBuffer.isView(payload)) {
    const view = payload as ArrayBufferView;
    if (view.byteLength !== expected) return null;
    if (view.buffer instanceof ArrayBuffer) {
      buffer = view.buffer.slice(view.byteOffset, view.byteOffset + view.byteLength);
    } else {
      return null;
    }
  } else {
    return null;
  }
  if (buffer.byteLength !== expected) return null;
  return { width, height, data: new Uint8ClampedArray(buffer) };
}

/** builtin 合成帧直接作为源像素；尺寸必须等于帧描述（builtin 固定 320×320），显示缩放留给 Canvas。 */
export function rgbaFrameFromBuiltin(frame: CatFrameRaster): RgbaFrame {
  if (
    !isSafeDimension(frame.width, MATERIAL_DISPLAY_MAX_DIMENSION) ||
    !isSafeDimension(frame.height, MATERIAL_DISPLAY_MAX_DIMENSION)
  ) {
    throw new RangeError('builtin frame dimensions are outside the bounded range');
  }
  if (frame.data.length !== frame.width * frame.height * 4) {
    throw new RangeError('builtin frame byte length does not match its dimensions');
  }
  return frame;
}

function sameDescriptorShape(
  left: MaterialFrameDescriptor,
  right: MaterialFrameDescriptor,
): boolean {
  return (
    left.source === right.source &&
    left.clipId === right.clipId &&
    left.frameIndex === right.frameIndex &&
    left.width === right.width &&
    left.height === right.height &&
    sameMaterialTriple(left, right)
  );
}

export interface PetFrameGateOptions {
  request: (frame: MaterialFrameDescriptor) => Promise<ArrayBuffer | Uint8Array>;
  onFrame: (frame: MaterialFrameDescriptor, payload: ArrayBuffer | Uint8Array) => void;
  onError: (frame: MaterialFrameDescriptor, error: unknown) => void;
}

/**
 * 有界取帧门：最多一个在途请求与一个最新待处理描述。新描述到达时只替换待处理项，
 * 在途结果若已不是当前描述或已有更新待处理项则直接丢弃；invalidate/dispose 之后
 * 的回包永不回填。失败不自动重试同一令牌。
 */
export class PetFrameRequestGate {
  private readonly options: PetFrameGateOptions;
  private current: MaterialFrameDescriptor | null = null;
  private pending: MaterialFrameDescriptor | null = null;
  private inFlight = false;
  private epoch = 0;
  private disposed = false;

  constructor(options: PetFrameGateOptions) {
    this.options = options;
  }

  get isDisposed(): boolean {
    return this.disposed;
  }

  get hasInFlight(): boolean {
    return this.inFlight;
  }

  get pendingDescriptor(): MaterialFrameDescriptor | null {
    return this.pending;
  }

  get currentDescriptor(): MaterialFrameDescriptor | null {
    return this.current;
  }

  offer(frame: MaterialFrameDescriptor): void {
    if (this.disposed) return;
    this.current = frame;
    if (this.inFlight) {
      this.pending = frame;
      return;
    }
    void this.run(frame);
  }

  /** 停止、卸载、切包或组件释放：使在途与待处理结果全部失效。 */
  invalidate(): void {
    this.epoch += 1;
    this.current = null;
    this.pending = null;
  }

  dispose(): void {
    if (this.disposed) return;
    this.disposed = true;
    this.invalidate();
  }

  private async run(initial: MaterialFrameDescriptor): Promise<void> {
    this.inFlight = true;
    try {
      let frame: MaterialFrameDescriptor | null = initial;
      while (frame !== null && !this.disposed) {
        const epoch = this.epoch;
        let payload: ArrayBuffer | Uint8Array | null = null;
        let failure: unknown = null;
        try {
          payload = await this.options.request(frame);
        } catch (error) {
          failure = error;
        }
        if (this.disposed) return;
        // 已有更新描述时，本次（无论成功失败）都已过期；只继续取最新一份。
        if (this.pending !== null) {
          frame = this.pending;
          this.pending = null;
          continue;
        }
        if (epoch === this.epoch && this.current !== null && sameDescriptorShape(frame, this.current)) {
          try {
            if (failure !== null) {
              this.options.onError(frame, failure);
            } else if (payload !== null) {
              this.options.onFrame(frame, payload);
            }
          } catch (error) {
            // 绘制/错误回调自身失败时不冒泡成未处理拒绝，也不让在途标记卡死；失败路径同样释放。
            if (failure === null) {
              try {
                this.options.onError(frame, error);
              } catch {
                // 固定错误回调也不可用时只结束本次请求，后续帧仍可继续。
              }
            }
          }
        }
        return;
      }
    } finally {
      // 即使绘制回调抛出，也不能让 fetch 门永久停在“在途”，否则后续帧全部被背压。
      this.inFlight = false;
    }
  }
}

/** 供 PetView 复验：宿主回包三元组必须与当前描述完全一致。 */
export function frameMatchesTriple(
  frame: MaterialFrameDescriptor,
  triple: MaterialFrameTriple,
): boolean {
  return sameMaterialTriple(frame, triple);
}
