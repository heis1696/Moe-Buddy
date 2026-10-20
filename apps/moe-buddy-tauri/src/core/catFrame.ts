/**
 * 纯代码预绘制四张合成猫帧（RGBA），不依赖 Canvas、网络或旧项目素材。
 * 四帧共用同一套刚体轮廓；只改变眼睛与嘴部像素，因此 alpha 通道逐像素一致。
 */

import { CAT_FRAME_COUNT } from './contract';
export { CAT_FRAME_COUNT } from './contract';

export interface CatFrameRaster {
  width: number;
  height: number;
  data: Uint8ClampedArray<ArrayBuffer>;
}

type Rgb = readonly [number, number, number];

const COLORS = {
  paper: [250, 247, 238],
  ear: [111, 143, 122],
  scarf: [84, 121, 100],
  moss: [56, 102, 83],
  ink: [38, 47, 43],
  blush: [235, 214, 201],
  highlight: [255, 255, 255],
  outline: [42, 70, 58],
} as const satisfies Record<string, Rgb>;

const SUBPIXEL_OFFSETS = [
  { x: 0.25, y: 0.25 },
  { x: 0.75, y: 0.25 },
  { x: 0.25, y: 0.75 },
  { x: 0.75, y: 0.75 },
] as const;

const EYE_CENTERS = [0.405, 0.595] as const;
const EYE_Y = 0.405;

function isIntegerSize(value: number): boolean {
  return Number.isInteger(value) && value > 0;
}

export function isCatFrameIndex(value: unknown): value is number {
  return (
    typeof value === 'number' &&
    Number.isInteger(value) &&
    value >= 0 &&
    value < CAT_FRAME_COUNT
  );
}

function insideEllipse(
  x: number,
  y: number,
  cx: number,
  cy: number,
  rx: number,
  ry: number,
): boolean {
  const dx = (x - cx) / rx;
  const dy = (y - cy) / ry;
  return dx * dx + dy * dy <= 1;
}

function triangleSign(
  px: number,
  py: number,
  ax: number,
  ay: number,
  bx: number,
  by: number,
): number {
  return (px - bx) * (ay - by) - (ax - bx) * (py - by);
}

function insideTriangle(
  x: number,
  y: number,
  ax: number,
  ay: number,
  bx: number,
  by: number,
  cx: number,
  cy: number,
): boolean {
  const d1 = triangleSign(x, y, ax, ay, bx, by);
  const d2 = triangleSign(x, y, bx, by, cx, cy);
  const d3 = triangleSign(x, y, cx, cy, ax, ay);
  const hasNegative = d1 < 0 || d2 < 0 || d3 < 0;
  const hasPositive = d1 > 0 || d2 > 0 || d3 > 0;
  return !(hasNegative && hasPositive);
}

function insideLeftEar(x: number, y: number): boolean {
  return insideTriangle(x, y, 0.235, 0.3, 0.3, 0.055, 0.435, 0.21);
}

function insideRightEar(x: number, y: number): boolean {
  return insideTriangle(x, y, 0.765, 0.3, 0.7, 0.055, 0.565, 0.21);
}

function insideEar(x: number, y: number): boolean {
  return insideLeftEar(x, y) || insideRightEar(x, y);
}

const TAIL_POINTS: ReadonlyArray<{ x: number; y: number }> = buildTailPoints();

function buildTailPoints(): Array<{ x: number; y: number }> {
  const points: Array<{ x: number; y: number }> = [];
  const segments = 20;
  for (let index = 0; index <= segments; index += 1) {
    const t = index / segments;
    const oneMinus = 1 - t;
    points.push({
      x: oneMinus * oneMinus * 0.7 + 2 * oneMinus * t * 0.95 + t * t * 0.86,
      y: oneMinus * oneMinus * 0.79 + 2 * oneMinus * t * 0.86 + t * t * 0.53,
    });
  }
  return points;
}

function insideTail(x: number, y: number): boolean {
  // 尾巴弯到右侧上方，形成与头部之间的一块透明缝，轮廓不是简单椭圆。
  if (x < 0.64 || x > 0.95 || y < 0.48 || y > 0.84) return false;
  const radiusSquared = 0.043 * 0.043;
  for (const point of TAIL_POINTS) {
    const dx = x - point.x;
    const dy = y - point.y;
    if (dx * dx + dy * dy <= radiusSquared) return true;
  }
  return false;
}

function insideSilhouette(x: number, y: number): boolean {
  if (x < 0.18 || x > 0.96 || y < 0.03 || y > 0.98) return false;
  if (insideEllipse(x, y, 0.5, 0.42, 0.3, 0.24)) return true;
  if (insideEllipse(x, y, 0.5, 0.71, 0.235, 0.225)) return true;
  if (insideEar(x, y)) return true;
  if (insideEllipse(x, y, 0.36, 0.915, 0.085, 0.04)) return true;
  if (insideEllipse(x, y, 0.64, 0.915, 0.085, 0.04)) return true;
  return insideTail(x, y);
}

function insideEye(x: number, y: number, frameIndex: number): boolean {
  for (const cx of EYE_CENTERS) {
    if (frameIndex === 1) {
      const t = (x - cx) / 0.05;
      if (Math.abs(t) > 1) continue;
      const arcY = EYE_Y - 0.018 * (1 - t * t);
      if (Math.abs(y - arcY) <= 0.009) return true;
    } else if (frameIndex === 2) {
      if (Math.abs(x - cx) <= 0.043 && Math.abs(y - (EYE_Y + 0.004)) <= 0.006) return true;
    } else if (insideEllipse(x, y, cx, EYE_Y, frameIndex === 3 ? 0.05 : 0.042, frameIndex === 3 ? 0.06 : 0.052)) {
      return true;
    }
  }
  return false;
}

function insideEyeHighlight(x: number, y: number, frameIndex: number): boolean {
  if (frameIndex !== 0 && frameIndex !== 3) return false;
  for (const cx of EYE_CENTERS) {
    const radius = frameIndex === 3 ? 0.013 : 0.01;
    if (insideEllipse(x, y, cx - 0.012, EYE_Y - 0.017, radius, radius)) return true;
  }
  return false;
}

function insideMouth(x: number, y: number, frameIndex: number): boolean {
  if (frameIndex === 1) return insideEllipse(x, y, 0.5, 0.514, 0.028, 0.016);
  if (frameIndex === 3) return insideEllipse(x, y, 0.5, 0.512, 0.011, 0.015);
  const t = (x - 0.5) / 0.03;
  if (Math.abs(t) > 1) return false;
  const arcY = 0.514 - 0.007 * (1 - t * t);
  return Math.abs(y - arcY) <= 0.0045;
}

function insideNose(x: number, y: number): boolean {
  return insideTriangle(x, y, 0.486, 0.466, 0.514, 0.466, 0.5, 0.486);
}

function insideCheek(x: number, y: number): boolean {
  return (
    insideEllipse(x, y, 0.325, 0.49, 0.035, 0.02) ||
    insideEllipse(x, y, 0.675, 0.49, 0.035, 0.02)
  );
}

function insideScarf(x: number, y: number): boolean {
  if (y >= 0.655 && y <= 0.688 && Math.abs(x - 0.5) <= 0.235) return true;
  return insideEllipse(x, y, 0.5, 0.694, 0.028, 0.022);
}

function featureColor(x: number, y: number, frameIndex: number): Rgb {
  if (insideEar(x, y)) return COLORS.ear;
  if (insideTail(x, y) && y < 0.6) return COLORS.moss;
  if (insideScarf(x, y)) return COLORS.scarf;
  if (insideCheek(x, y)) return COLORS.blush;
  if (insideEyeHighlight(x, y, frameIndex)) return COLORS.highlight;
  if (insideEye(x, y, frameIndex)) return COLORS.ink;
  if (insideNose(x, y)) return COLORS.moss;
  if (insideMouth(x, y, frameIndex)) return COLORS.ink;
  return COLORS.paper;
}

function isSilhouetteEdge(x: number, y: number, step: number): boolean {
  return (
    !insideSilhouette(x - step, y) ||
    !insideSilhouette(x + step, y) ||
    !insideSilhouette(x, y - step) ||
    !insideSilhouette(x, y + step)
  );
}

export function renderCatFrame(width: number, height: number, frameIndex: number): CatFrameRaster {
  if (!isIntegerSize(width) || !isIntegerSize(height)) {
    throw new RangeError('cat frame size must be a positive integer');
  }
  if (!isCatFrameIndex(frameIndex)) {
    throw new RangeError('cat frame index must be 0..3');
  }

  const size = Math.min(width, height);
  const offsetX = (width - size) / 2;
  const offsetY = (height - size) / 2;
  const outlineStep = 2.2 / size;
  const data = new Uint8ClampedArray(width * height * 4);

  for (let py = 0; py < height; py += 1) {
    for (let px = 0; px < width; px += 1) {
      let coveredSamples = 0;
      for (const offset of SUBPIXEL_OFFSETS) {
        const nx = (px + offset.x - offsetX) / size;
        const ny = (py + offset.y - offsetY) / size;
        if (insideSilhouette(nx, ny)) coveredSamples += 1;
      }
      if (coveredSamples === 0) continue;

      const nx = (px + 0.5 - offsetX) / size;
      const ny = (py + 0.5 - offsetY) / size;
      const inside = insideSilhouette(nx, ny);
      const color = inside && isSilhouetteEdge(nx, ny, outlineStep)
        ? COLORS.outline
        : featureColor(nx, ny, frameIndex);
      const target = (py * width + px) * 4;
      data[target] = color[0];
      data[target + 1] = color[1];
      data[target + 2] = color[2];
      data[target + 3] = Math.round((coveredSamples / SUBPIXEL_OFFSETS.length) * 255);
    }
  }

  return { width, height, data };
}
