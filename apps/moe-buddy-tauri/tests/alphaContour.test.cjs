const test = require('node:test');
const assert = require('node:assert/strict');

const { CAT_FRAME_COUNT, renderCatFrame } = require('../.test-build/catFrame.js');
const {
  HIT_MASK_MAX_DIMENSION,
  buildHitMask,
  parseHitMaskError,
} = require('../.test-build/hitMask.js');

function alphaOf(frame) {
  const alpha = new Uint8ClampedArray(frame.width * frame.height);
  for (let pixel = 0; pixel < alpha.length; pixel += 1) {
    alpha[pixel] = frame.data[pixel * 4 + 3];
  }
  return alpha;
}

function countRunsInRow(frame, row) {
  let runs = 0;
  let inside = false;
  for (let x = 0; x < frame.width; x += 1) {
    const opaque = frame.data[(row * frame.width + x) * 4 + 3] > 0;
    if (opaque && !inside) runs += 1;
    inside = opaque;
  }
  return runs;
}

test('四帧共享同一不规则 alpha 轮廓，仅表情像素不同', () => {
  const frames = Array.from({ length: CAT_FRAME_COUNT }, (_, index) =>
    renderCatFrame(160, 160, index),
  );
  const baseAlpha = alphaOf(frames[0]);
  for (const frame of frames.slice(1)) {
    assert.deepEqual(alphaOf(frame), baseAlpha);
  }

  // 耳朵在约 10% 高度形成左右两段，证明轮廓不是单一椭圆。
  assert.equal(countRunsInRow(frames[0], 16), 2);
  // 头顶中心不透明、左上角完全透明。
  assert.equal(frames[0].data[(80 * 160 + 80) * 4 + 3], 255);
  assert.equal(frames[0].data[3], 0);

  // 轮廓一致，但眼睛/嘴的 RGB 必须确有差异。
  const differs = frames[0].data.some((value, index) => {
    if (index % 4 === 3) return false;
    return value !== frames[1].data[index];
  });
  assert.equal(differs, true);
});

test('alpha 网格覆盖整个窗口显示区域，不遗漏任何不透明像素', () => {
  const frame = renderCatFrame(320, 240, 2);
  const mask = buildHitMask(frame);

  assert.ok(mask.width <= HIT_MASK_MAX_DIMENSION);
  assert.ok(mask.height <= HIT_MASK_MAX_DIMENSION);
  assert.equal(mask.alpha.length, mask.width * mask.height);
  assert.ok(mask.alpha.every((value) => Number.isInteger(value) && value >= 0 && value <= 255));
  assert.ok(mask.alpha.some((value) => value > 0));
  assert.ok(mask.alpha.some((value) => value === 0));

  const covered = new Uint8Array(frame.width * frame.height);
  for (let maskY = 0; maskY < mask.height; maskY += 1) {
    const y0 = Math.floor((maskY * frame.height) / mask.height);
    const y1 = Math.max(y0 + 1, Math.floor(((maskY + 1) * frame.height) / mask.height));
    for (let maskX = 0; maskX < mask.width; maskX += 1) {
      const value = mask.alpha[maskY * mask.width + maskX];
      if (value === 0) continue;
      const x0 = Math.floor((maskX * frame.width) / mask.width);
      const x1 = Math.max(x0 + 1, Math.floor(((maskX + 1) * frame.width) / mask.width));
      for (let y = y0; y < y1; y += 1) {
        for (let x = x0; x < x1; x += 1) {
          covered[y * frame.width + x] = 1;
        }
      }
    }
  }

  let opaquePixels = 0;
  for (let pixel = 0; pixel < covered.length; pixel += 1) {
    if (frame.data[pixel * 4 + 3] > 0) {
      opaquePixels += 1;
      assert.equal(covered[pixel], 1, `pixel ${pixel} missing from hit mask`);
    }
  }
  assert.ok(opaquePixels > 0);
});

test('网格保留窗口宽高比，四帧命中网格完全一致', () => {
  const wide = buildHitMask(renderCatFrame(320, 160, 0));
  assert.equal(wide.width, 256);
  assert.equal(wide.height, 128);

  const narrow = buildHitMask(renderCatFrame(160, 320, 0));
  assert.equal(narrow.width, 128);
  assert.equal(narrow.height, 256);

  const masks = [0, 1, 2, 3].map((index) => buildHitMask(renderCatFrame(160, 160, index)));
  for (const mask of masks.slice(1)) {
    assert.deepEqual(mask.alpha, masks[0].alpha);
  }
});

test('非法尺寸/索引不会悄悄生成假帧', () => {
  assert.throws(() => renderCatFrame(0, 100, 0), RangeError);
  assert.throws(() => renderCatFrame(100, 100, 4), RangeError);
  assert.throws(() => buildHitMask(renderCatFrame(10, 10, 0), 0), RangeError);
  assert.throws(() => buildHitMask(renderCatFrame(10, 10, 0), 257), RangeError);
});

test('穿透失败事件只映射固定安全文案，不回显宿主原始正文', () => {
  assert.match(parseHitMaskError('cursor_query'), /点击穿透/);
  assert.match(parseHitMaskError('window_style'), /点击穿透/);
  for (const payload of [null, 1, {}, 'boom: raw host message', 'window_style: stack']) {
    assert.equal(parseHitMaskError(payload), null);
  }
});
