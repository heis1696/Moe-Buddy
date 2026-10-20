import type { CatFrameRaster } from './catFrame';
import type { RgbaFrame } from './materialFrame';
import type { HitMaskPayload, MaterialFrameTriple } from './types';

export const HIT_MASK_MAX_DIMENSION = 256;

export interface HitMaskGrid {
  width: number;
  height: number;
  alpha: number[];
}

/** 源帧在显示 backing 中的实际绘制矩形；留边区域不属于命中区域。 */
export interface MaskDisplay {
  width: number;
  height: number;
  placement: {
    offsetX: number;
    offsetY: number;
    width: number;
    height: number;
  };
}

type AlphaFrame = Pick<CatFrameRaster, 'width' | 'height' | 'data'> | RgbaFrame;

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, value));
}

/**
 * 把实际绘制的帧 alpha 压成不超过 256 的单通道网格。
 * - 每个网格取覆盖区域（映射回源像素）的最大 alpha，保证不遗漏可交互像素；
 * - 正式包按 `MaskDisplay.placement` 只采样真正绘制到屏幕上的区域，
 *   等比缩放后的透明留边不会被误算为命中；
 * - 省略 display 时等价于“源帧铺满整个显示区域”，保留 builtin 的既有行为。
 */
export function buildHitMask(
  frame: AlphaFrame,
  maxDimension: number = HIT_MASK_MAX_DIMENSION,
  display?: MaskDisplay,
): HitMaskGrid {
  if (!Number.isInteger(maxDimension) || maxDimension < 1 || maxDimension > HIT_MASK_MAX_DIMENSION) {
    throw new RangeError(`hit mask dimension budget must be 1..${HIT_MASK_MAX_DIMENSION}`);
  }
  if (frame.width < 1 || frame.height < 1) {
    throw new RangeError('frame must not be empty');
  }

  const displayWidth = display?.width ?? frame.width;
  const displayHeight = display?.height ?? frame.height;
  const placement = display?.placement ?? {
    offsetX: 0,
    offsetY: 0,
    width: frame.width,
    height: frame.height,
  };
  if (
    !Number.isInteger(displayWidth) ||
    !Number.isInteger(displayHeight) ||
    displayWidth < 1 ||
    displayHeight < 1 ||
    !Number.isInteger(placement.offsetX) ||
    !Number.isInteger(placement.offsetY) ||
    !Number.isInteger(placement.width) ||
    !Number.isInteger(placement.height) ||
    placement.width < 1 ||
    placement.height < 1 ||
    placement.offsetX < 0 ||
    placement.offsetY < 0 ||
    placement.offsetX + placement.width > displayWidth ||
    placement.offsetY + placement.height > displayHeight
  ) {
    throw new RangeError('display placement is outside the bounded display area');
  }

  const scale = Math.min(1, maxDimension / Math.max(displayWidth, displayHeight));
  const width = Math.max(1, Math.min(maxDimension, Math.round(displayWidth * scale)));
  const height = Math.max(1, Math.min(maxDimension, Math.round(displayHeight * scale)));
  const alpha = new Array<number>(width * height).fill(0);

  for (let maskY = 0; maskY < height; maskY += 1) {
    const displayY0 = Math.floor((maskY * displayHeight) / height);
    const displayY1 = Math.max(displayY0 + 1, Math.ceil(((maskY + 1) * displayHeight) / height));
    const intersectY0 = Math.max(displayY0, placement.offsetY);
    const intersectY1 = Math.min(displayY1, placement.offsetY + placement.height);
    if (intersectY0 >= intersectY1) continue;
    const sourceY0 = clamp(
      Math.floor(((intersectY0 - placement.offsetY) * frame.height) / placement.height),
      0,
      frame.height - 1,
    );
    const sourceY1 = clamp(
      Math.max(sourceY0 + 1, Math.floor(((intersectY1 - placement.offsetY) * frame.height) / placement.height)),
      sourceY0 + 1,
      frame.height,
    );

    for (let maskX = 0; maskX < width; maskX += 1) {
      const displayX0 = Math.floor((maskX * displayWidth) / width);
      const displayX1 = Math.max(displayX0 + 1, Math.ceil(((maskX + 1) * displayWidth) / width));
      const intersectX0 = Math.max(displayX0, placement.offsetX);
      const intersectX1 = Math.min(displayX1, placement.offsetX + placement.width);
      if (intersectX0 >= intersectX1) continue;
      const sourceX0 = clamp(
        Math.floor(((intersectX0 - placement.offsetX) * frame.width) / placement.width),
        0,
        frame.width - 1,
      );
      const sourceX1 = clamp(
        Math.max(sourceX0 + 1, Math.floor(((intersectX1 - placement.offsetX) * frame.width) / placement.width)),
        sourceX0 + 1,
        frame.width,
      );

      let maximum = 0;
      for (let y = sourceY0; y < sourceY1; y += 1) {
        for (let x = sourceX0; x < sourceX1; x += 1) {
          const value = frame.data[(y * frame.width + x) * 4 + 3];
          if (value > maximum) maximum = value;
        }
      }
      alpha[maskY * width + maskX] = maximum;
    }
  }

  return { width, height, alpha };
}

/** 显式 empty：1×1 零 alpha，配合空帧三元组；宿主拒绝 empty 下非零 alpha。 */
export function emptyHitMaskGrid(): HitMaskGrid {
  return { width: 1, height: 1, alpha: [0] };
}

/** 命中掩码必须携带产生它的当前帧三元组。 */
export function attachMaskFrame(grid: HitMaskGrid, frame: MaterialFrameTriple): HitMaskPayload {
  return { width: grid.width, height: grid.height, alpha: grid.alpha, frame };
}

/** 宿主异步上报命中/穿透更新失败时只显示这条固定安全文案。 */
const HIT_MASK_ERROR_TEXT = '宠物点击穿透状态更新失败，请重启 Moe Buddy。';

/**
 * 只接受宿主固定的错误码，统一映射为固定安全文案；任何原始错误正文都会被丢弃，
 * 不进入可见界面。
 */
export function parseHitMaskError(payload: unknown): string | null {
  if (payload === 'cursor_query' || payload === 'window_style') {
    return HIT_MASK_ERROR_TEXT;
  }
  return null;
}
