const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');

// Execute the actual PetView <script setup> with counted Canvas/RGBA stand-ins.
// This reproduces the lead's real-component probe (320x320 window, DPR=4, two
// builtin frames) and covers stop/unload plus late package results. It is a
// component allocation/size regression, not a GPU/RSS measurement.
function component() {
  const sourcePath = path.join(__dirname, '../src/app/PetView.vue');
  const source = fs
    .readFileSync(sourcePath, 'utf8')
    .match(/<script setup lang="ts">([\s\S]*?)<\/script>/)[1];
  const script = ts.transpileModule(
    source +
      `
    module.exports.probe = {
      canvasRef, frame, frameError, maskError, playbackFault,
      handleFrame, clearCanvas, frameGate,
      currentSource: () => sourceFrame,
      currentSourceCanvas: () => sourceCanvas,
      currentDisplaySize: () => displaySize,
      currentPlacement: () => placement,
      currentTriple: () => currentTriple,
    };
  `,
    { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } },
  ).outputText;

  const canvases = [];
  const draws = [];
  const pixelRequests = [];
  const maskCalls = [];
  let peakBytes = 0;
  let probe = null;

  function canvasBytes() {
    return canvases.reduce((sum, canvas) => sum + canvas.width * canvas.height * 4, 0);
  }

  function makeCanvas() {
    const canvas = {
      width: 0,
      height: 0,
      getContext() {
        return {
          canvas,
          clearRect() {},
          drawImage(image, dx, dy, dw, dh) {
            draws.push({ image, dx, dy, dw, dh });
          },
          putImageData(image) {
            const oldSource = probe?.currentSource()?.data.byteLength ?? 0;
            peakBytes = Math.max(peakBytes, canvasBytes() + oldSource + image.data.byteLength);
          },
        };
      },
    };
    canvases.push(canvas);
    return canvas;
  }

  const exported = { exports: {} };
  const platform = {
    setHitMask: async (mask) => {
      maskCalls.push(mask);
    },
    requestMaterialPixels: (triple) =>
      new Promise((resolve, reject) => pixelRequests.push({ triple, resolve, reject })),
    listenFrameEvents: async () => () => {},
    listenHitMaskErrorEvents: async () => () => {},
    requestFrame: async () => {
      throw new Error('requestFrame is not used by this probe');
    },
    invokeWindowCommand: async () => {},
  };

  const context = {
    module: exported,
    exports: exported.exports,
    setTimeout,
    clearTimeout,
    window: {
      innerWidth: 320,
      innerHeight: 320,
      devicePixelRatio: 4,
      matchMedia: () => ({ addEventListener() {}, removeEventListener() {} }),
    },
    document: {
      createElement: (tag) => {
        if (tag !== 'canvas') throw new Error(`Unexpected element: ${tag}`);
        return makeCanvas();
      },
      documentElement: { dataset: {} },
    },
    ImageData: class {
      constructor(data, width, height) {
        this.data = data;
        this.width = width;
        this.height = height;
      }
    },
    require(name) {
      if (name === 'vue') {
        return { ...vue, onMounted() {}, onBeforeUnmount() {} };
      }
      if (name === '../platform/tauri') return platform;
      if (name.startsWith('../core/')) {
        return require(path.join(__dirname, '../.test-build', name.slice('../core/'.length) + '.js'));
      }
      throw new Error(`Unexpected component dependency: ${name}`);
    },
  };
  vm.runInNewContext(script, context, { filename: sourcePath });
  probe = exported.exports.probe;
  probe.canvasRef.value = makeCanvas();

  return {
    api: probe,
    canvases,
    draws,
    pixelRequests,
    maskCalls,
    peakBytes: () => peakBytes,
    canvasBytes,
  };
}

function builtinFrame(presentationSequence, frameIndex) {
  return {
    resourceGeneration: 1,
    playbackGeneration: 1,
    presentationSequence,
    source: 'builtin',
    clipId: 'idle-four-frame',
    frameIndex,
    width: 320,
    height: 320,
    paused: false,
    faulted: false,
  };
}

function packageFrame(presentationSequence, overrides = {}) {
  return {
    resourceGeneration: 1,
    playbackGeneration: 1,
    presentationSequence,
    source: 'package',
    clipId: 'idle',
    frameIndex: 0,
    width: 512,
    height: 512,
    paused: false,
    faulted: false,
    ...overrides,
  };
}

function emptyFrame(presentationSequence, overrides = {}) {
  return {
    resourceGeneration: 1,
    playbackGeneration: 1,
    presentationSequence,
    source: 'empty',
    clipId: null,
    frameIndex: null,
    width: 0,
    height: 0,
    paused: false,
    faulted: false,
    ...overrides,
  };
}

function rgba(width, height, value) {
  const data = new Uint8Array(width * height * 4);
  for (let pixel = 0; pixel < width * height; pixel += 1) {
    data[pixel * 4] = value;
    data[pixel * 4 + 1] = value;
    data[pixel * 4 + 2] = value;
    data[pixel * 4 + 3] = 255;
  }
  return data.buffer;
}

const tick = () => new Promise((resolve) => setImmediate(resolve));
async function flush() {
  await tick();
  await tick();
}

test('真实 PetView：320×320@DPR4 两次 builtin 帧的源保持 320×320，峰值低于 16MiB', () => {
  const instance = component();
  const api = instance.api;
  api.handleFrame(builtinFrame(1, 0));

  assert.equal(api.currentDisplaySize().width, 1024);
  assert.equal(api.currentDisplaySize().height, 1024);
  assert.equal(api.currentSource().width, 320);
  assert.equal(api.currentSource().height, 320);
  assert.equal(api.currentSource().data.byteLength, 320 * 320 * 4);
  assert.equal(api.currentSourceCanvas().width, 320);
  assert.equal(api.currentSourceCanvas().height, 320);
  assert.deepEqual(api.currentPlacement(), { offsetX: 0, offsetY: 0, width: 1024, height: 1024 });
  assert.equal(api.canvasRef.value.width, 1024);
  assert.equal(api.canvasRef.value.height, 1024);
  assert.equal(instance.draws.at(-1).dw, 1024);
  assert.equal(instance.draws.at(-1).dh, 1024);

  api.handleFrame(builtinFrame(2, 1));
  assert.equal(api.currentSource().width, 320);
  assert.equal(api.currentSource().height, 320);
  assert.equal(api.currentSourceCanvas().width, 320);
  assert.equal(api.currentSourceCanvas().height, 320);
  assert.equal(api.canvasRef.value.width, 1024);

  // 显示 Canvas 4MiB + 新旧源 Canvas 各 0.39MiB + 新旧源 RGBA 各 0.39MiB。
  const expectedPeak = 1024 * 1024 * 4 + 4 * (320 * 320 * 4);
  assert.equal(instance.peakBytes(), expectedPeak);
  assert.ok(
    instance.peakBytes() <= 16 * 1024 * 1024,
    `controlled presentation peak ${instance.peakBytes()} exceeds 16MiB`,
  );
});

test('真实 PetView：正式包帧按源尺寸缓存并只放大显示 Canvas；坏回包保留当前帧', async () => {
  const instance = component();
  const api = instance.api;
  api.handleFrame(packageFrame(1));
  assert.equal(instance.pixelRequests.length, 1);
  assert.deepEqual(instance.pixelRequests[0].triple, {
    resourceGeneration: 1,
    playbackGeneration: 1,
    presentationSequence: 1,
  });
  instance.pixelRequests[0].resolve(rgba(512, 512, 10));
  await flush();

  assert.equal(api.currentSource().width, 512);
  assert.equal(api.currentSource().height, 512);
  assert.equal(api.currentSourceCanvas().width, 512);
  assert.equal(api.currentPlacement().width, 1024);
  assert.equal(api.currentSource().data[0], 10);
  const peakAfterFirst = instance.peakBytes();

  // 同代际下一帧回包尺寸/字节不符：报错但不得丢弃仍然有效的当前源帧。
  api.handleFrame(packageFrame(2, { frameIndex: 1 }));
  assert.equal(instance.pixelRequests.length, 2);
  instance.pixelRequests[1].resolve(new ArrayBuffer(16));
  await flush();
  assert.equal(api.currentSource().data[0], 10);
  assert.equal(api.currentSource().width, 512);
  assert.match(api.frameError.value, /尺寸或字节数不匹配/);
  assert.equal(instance.peakBytes(), peakAfterFirst);
});

test('真实 PetView：builtin 使在途包回包失效；停止/卸载清空 Canvas 与像素引用', async () => {
  const instance = component();
  const api = instance.api;
  api.handleFrame(packageFrame(1));
  instance.pixelRequests[0].resolve(rgba(512, 512, 20));
  await flush();
  assert.equal(api.currentSource().data[0], 20);

  // 切回 builtin：旧包在途回包不得复活旧像素。
  api.handleFrame(builtinFrame(2, 2));
  assert.equal(api.currentSource().width, 320);
  api.handleFrame(packageFrame(3));
  const late = instance.pixelRequests.at(-1);
  api.handleFrame(builtinFrame(4, 3));
  late.resolve(rgba(512, 512, 30));
  await flush();
  assert.equal(api.currentSource().width, 320, '迟到包帧不得替换 builtin 源');
  assert.equal(api.currentSource().data[0] === 30, false);

  // 停止/卸载都发布显式 empty，清除显示与源 Canvas 尺寸并丢弃像素引用。
  const canvasesBefore = instance.canvases.length;
  api.handleFrame(emptyFrame(5));
  assert.equal(api.currentSource(), null);
  assert.equal(api.currentSourceCanvas(), null);
  assert.equal(api.canvasRef.value.width, 0);
  assert.equal(api.canvasRef.value.height, 0);
  assert.equal(api.frameError.value, null);
  assert.equal(instance.canvases.length, canvasesBefore, 'empty 不得新建 Canvas');
  assert.equal(instance.canvasBytes(), 0);
});

test('真实 PetView：切包后旧包迟到回包只释放不绘制，新包回包正常生效', async () => {
  const instance = component();
  const api = instance.api;
  api.handleFrame(packageFrame(1));
  assert.equal(instance.pixelRequests.length, 1);

  // 新播放代际：旧请求继续在途，但只保留最新待处理描述。
  api.handleFrame(packageFrame(2, { playbackGeneration: 2 }));
  assert.equal(api.currentSource(), null, '切包后旧源已释放，等待新包帧');
  const oldRequest = instance.pixelRequests[0];
  oldRequest.resolve(rgba(512, 512, 40));
  await flush();
  assert.equal(instance.pixelRequests.length, 2, '旧回包后只发起最新一份');
  assert.equal(api.currentSource(), null, '旧包迟到回包不得绘制');

  instance.pixelRequests[1].resolve(rgba(512, 512, 50));
  await flush();
  assert.equal(api.currentSource().data[0], 50);
  assert.equal(api.currentTriple().playbackGeneration, 2);
});
