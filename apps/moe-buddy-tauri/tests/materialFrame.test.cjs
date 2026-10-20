const test = require('node:test');
const assert = require('node:assert/strict');

const {
  MATERIAL_BINARY_MAX_BYTES,
  MATERIAL_DISPLAY_MAX_DIMENSION,
  MATERIAL_PRESENTATION_BUDGET_BYTES,
  PetFrameRequestGate,
  displayBackingSize,
  frameMatchesTriple,
  framePlacement,
  rgbaFrameFromBinary,
} = require('../.test-build/materialFrame.js');
const {
  HIT_MASK_MAX_DIMENSION,
  attachMaskFrame,
  buildHitMask,
  emptyHitMaskGrid,
} = require('../.test-build/hitMask.js');
const { renderCatFrame } = require('../.test-build/catFrame.js');

function packageDescriptor(overrides = {}) {
  return {
    resourceGeneration: 1,
    playbackGeneration: 1,
    presentationSequence: 1,
    source: 'package',
    clipId: 'idle',
    frameIndex: 0,
    width: 2,
    height: 2,
    paused: false,
    faulted: false,
    ...overrides,
  };
}

function rgbaBytes(width, height, alphaAt = () => 255) {
  const data = new Uint8Array(width * height * 4);
  for (let pixel = 0; pixel < width * height; pixel += 1) {
    data[pixel * 4] = 10;
    data[pixel * 4 + 1] = 20;
    data[pixel * 4 + 2] = 30;
    data[pixel * 4 + 3] = alphaAt(pixel);
  }
  return data;
}

function controlledGate() {
  const calls = [];
  const applied = [];
  const errors = [];
  const gate = new PetFrameRequestGate({
    request: (frame) =>
      new Promise((resolve, reject) => {
        calls.push({ frame, resolve, reject });
      }),
    onFrame: (frame, payload) => applied.push({ frame, payload }),
    onError: (frame, error) => errors.push({ frame, error }),
  });
  return { calls, applied, errors, gate };
}

test('显示 backing 等比压到宽高各至多 1024，非法窗口尺寸明确报错', () => {
  assert.deepEqual(displayBackingSize(800, 600, 1), { width: 800, height: 600 });
  assert.deepEqual(displayBackingSize(1000, 500, 2), { width: 1024, height: 512 });
  assert.deepEqual(displayBackingSize(1024, 1024, 2), { width: 1024, height: 1024 });
  const huge = displayBackingSize(3000, 2000, 1);
  assert.ok(huge.width <= MATERIAL_DISPLAY_MAX_DIMENSION);
  assert.ok(huge.height <= MATERIAL_DISPLAY_MAX_DIMENSION);
  assert.deepEqual(huge, { width: 1024, height: 683 });
  assert.ok(huge.width * huge.height * 4 <= MATERIAL_PRESENTATION_BUDGET_BYTES);

  for (const args of [
    [0, 600, 1],
    [800, -1, 1],
    [Number.NaN, 600, 1],
  ]) {
    assert.throws(() => displayBackingSize(...args), RangeError);
  }
});

test('正式包帧等比 contain 居中；越界尺寸明确报错', () => {
  assert.deepEqual(framePlacement(512, 256, 1024, 1024), {
    offsetX: 0,
    offsetY: 256,
    width: 1024,
    height: 512,
  });
  assert.deepEqual(framePlacement(100, 200, 1024, 600), {
    offsetX: 362,
    offsetY: 0,
    width: 300,
    height: 600,
  });
  assert.throws(() => framePlacement(0, 10, 100, 100), RangeError);
  assert.throws(() => framePlacement(10, 10, 0, 100), RangeError);
  assert.throws(() => framePlacement(513, 10, 100, 100), RangeError);
  assert.throws(() => framePlacement(10, 10, 1025, 100), RangeError);
});

test('单帧二进制长度必须精确匹配 width×height×4 且至多 1MiB', () => {
  const bytes = rgbaBytes(2, 2);
  const frame = rgbaFrameFromBinary(bytes.buffer, 2, 2);
  assert.equal(frame.width, 2);
  assert.equal(frame.height, 2);
  assert.equal(frame.data.length, 16);
  assert.equal(frame.data[3], 255);

  const offset = new Uint8Array(bytes.buffer.slice(0)).subarray(4, 8);
  const fromView = rgbaFrameFromBinary(offset, 1, 1);
  assert.equal(fromView.data.length, 4);

  assert.equal(rgbaFrameFromBinary(new ArrayBuffer(15), 2, 2), null);
  assert.equal(rgbaFrameFromBinary(new ArrayBuffer(17), 2, 2), null);
  assert.equal(rgbaFrameFromBinary('not bytes', 2, 2), null);
  assert.equal(rgbaFrameFromBinary(new ArrayBuffer(4), 0, 1), null);
  assert.equal(rgbaFrameFromBinary(new ArrayBuffer(4), 513, 1), null);
  const max = rgbaFrameFromBinary(new ArrayBuffer(512 * 512 * 4), 512, 512);
  assert.equal(max.data.length, MATERIAL_BINARY_MAX_BYTES);
  assert.equal(rgbaFrameFromBinary(new ArrayBuffer(512 * 513 * 4), 512, 513), null);
});

test('取帧门最多一个在途请求：更新描述只保留最新一份，旧回包不回填', async () => {
  const { calls, applied, errors, gate } = controlledGate();
  const first = packageDescriptor({ presentationSequence: 1 });
  const second = packageDescriptor({ presentationSequence: 2, frameIndex: 1 });
  const third = packageDescriptor({ presentationSequence: 3, frameIndex: 2 });

  gate.offer(first);
  assert.equal(calls.length, 1);
  assert.equal(gate.hasInFlight, true);

  gate.offer(second);
  gate.offer(third);
  assert.equal(calls.length, 1, '在途期间不得并发第二个请求');
  assert.equal(gate.pendingDescriptor.presentationSequence, 3, '待处理项只保留最新描述');

  calls[0].resolve(rgbaBytes(2, 2).buffer);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(applied.length, 0, '已被更新描述取代的在途结果不得绘制');
  assert.equal(calls.length, 2);
  assert.equal(calls[1].frame.presentationSequence, 3);

  calls[1].resolve(rgbaBytes(2, 2).buffer);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(applied.length, 1);
  assert.equal(applied[0].frame.presentationSequence, 3);
  assert.equal(errors.length, 0);
  assert.equal(gate.hasInFlight, false);
});

test('卸载/停止使在途回包失效；释放后旧结果不绘制且不再发起', async () => {
  const { calls, applied, gate } = controlledGate();
  gate.offer(packageDescriptor({ presentationSequence: 1 }));
  gate.invalidate();
  calls[0].resolve(rgbaBytes(2, 2).buffer);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(applied.length, 0, '失效后的回包不得回填');
  assert.equal(gate.hasInFlight, false);

  gate.offer(packageDescriptor({ presentationSequence: 2 }));
  assert.equal(calls.length, 2);
  gate.dispose();
  calls[1].resolve(rgbaBytes(2, 2).buffer);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(applied.length, 0);
  assert.equal(gate.isDisposed, true);

  gate.offer(packageDescriptor({ presentationSequence: 3 }));
  assert.equal(calls.length, 2, '释放后不得再发起取帧');
});
test('失败不自动重试同一令牌；已有更新待处理描述时直接改取最新一份', async () => {
  const { calls, applied, errors, gate } = controlledGate();
  const first = packageDescriptor({ presentationSequence: 1 });
  const next = packageDescriptor({ presentationSequence: 2 });
  gate.offer(first);
  gate.offer(next);
  calls[0].reject(new Error('synthetic pixel failure'));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(errors.length, 0, '过期失败不回报到当前界面');
  assert.equal(calls.length, 2);
  assert.equal(calls[1].frame.presentationSequence, 2);

  calls[1].reject(new Error('latest failure'));
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(errors.length, 1);
  assert.equal(errors[0].frame.presentationSequence, 2);
  assert.equal(applied.length, 0);
  assert.equal(calls.length, 2, '失败后不得自动重试同一令牌');
});

test('绘制回调抛出时取帧门不在途卡死，且不产生未处理拒绝', async () => {
  const calls = [];
  const errors = [];
  const gate = new PetFrameRequestGate({
    request: (frame) =>
      new Promise((resolve) => {
        calls.push({ frame, resolve });
      }),
    onFrame: () => {
      throw new Error('synthetic draw failure');
    },
    onError: (frame, error) => errors.push({ frame, error }),
  });
  gate.offer(packageDescriptor({ presentationSequence: 1 }));
  calls[0].resolve(rgbaBytes(2, 2).buffer);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(gate.hasInFlight, false);
  assert.equal(errors.length, 1, '绘制失败通过固定错误回调收口');

  const next = packageDescriptor({ presentationSequence: 2 });
  gate.offer(next);
  assert.equal(calls.length, 2, '回调失败后仍可继续取下一帧');
  calls[1].resolve(rgbaBytes(2, 2).buffer);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(gate.hasInFlight, false);
  gate.dispose();
});

test('按真实 alpha 生成有界命中网格；等比留边透明不算命中', () => {
  const frame = {
    width: 2,
    height: 2,
    data: new Uint8ClampedArray(rgbaBytes(2, 2, (pixel) => (pixel === 3 ? 0 : 255))),
  };
  const full = buildHitMask(frame, 2);
  assert.deepEqual(full, { width: 2, height: 2, alpha: [255, 255, 255, 0] });

  const placed = buildHitMask(frame, 4, {
    width: 4,
    height: 4,
    placement: { offsetX: 1, offsetY: 1, width: 2, height: 2 },
  });
  assert.equal(placed.width, 4);
  assert.equal(placed.height, 4);
  for (let y = 0; y < 4; y += 1) {
    for (let x = 0; x < 4; x += 1) {
      const value = placed.alpha[y * 4 + x];
      const inside = x >= 1 && x <= 2 && y >= 1 && y <= 2;
      if (!inside) assert.equal(value, 0, `transparent letterbox (${x},${y})`);
    }
  }
  assert.equal(placed.alpha[1 * 4 + 1], 255);
  assert.equal(placed.alpha[2 * 4 + 2], 0, '零 alpha 源像素不得被掩码算作命中');
});

test('掩码网格不超过 256 且保留 builtin 全窗口覆盖行为', () => {
  const frame = renderCatFrame(320, 240, 2);
  const mask = buildHitMask(frame);
  assert.ok(mask.width <= HIT_MASK_MAX_DIMENSION);
  assert.ok(mask.height <= HIT_MASK_MAX_DIMENSION);
  assert.equal(mask.alpha.length, mask.width * mask.height);
  assert.ok(mask.alpha.some((value) => value > 0));
  assert.ok(mask.alpha.some((value) => value === 0));
});

test('显式 empty 使用 1×1 零 alpha，掩码携带当前帧三元组', () => {
  const empty = emptyHitMaskGrid();
  assert.deepEqual(empty, { width: 1, height: 1, alpha: [0] });
  const payload = attachMaskFrame(empty, {
    resourceGeneration: 4,
    playbackGeneration: 5,
    presentationSequence: 9,
  });
  assert.deepEqual(payload, {
    width: 1,
    height: 1,
    alpha: [0],
    frame: {
      resourceGeneration: 4,
      playbackGeneration: 5,
      presentationSequence: 9,
    },
  });
  assert.equal(frameMatchesTriple(packageDescriptor(), {
    resourceGeneration: 1,
    playbackGeneration: 1,
    presentationSequence: 1,
  }), true);
  assert.equal(frameMatchesTriple(packageDescriptor(), {
    resourceGeneration: 1,
    playbackGeneration: 1,
    presentationSequence: 2,
  }), false);
});
