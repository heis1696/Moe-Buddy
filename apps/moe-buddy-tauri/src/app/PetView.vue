<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { renderCatFrame } from '../core/catFrame';
import { attachMaskFrame, buildHitMask, emptyHitMaskGrid } from '../core/hitMask';
import { releaseListeners, type Unlisten } from '../core/listeners';
import {
  materialFrameTriple,
  sameMaterialTriple,
  type MaterialFrameDescriptor,
} from '../core/material';
import {
  PetFrameRequestGate,
  displayBackingSize,
  framePlacement,
  rgbaFrameFromBinary,
  rgbaFrameFromBuiltin,
  type DisplaySize,
  type FramePlacement,
  type RgbaFrame,
} from '../core/materialFrame';
import { MaskSyncQueue } from '../core/maskSync';
import {
  invokeWindowCommand,
  listenFrameEvents,
  listenHitMaskErrorEvents,
  requestFrame,
  requestMaterialPixels,
  setHitMask,
} from '../platform/tauri';
import type { MaterialFrameTriple } from '../core/types';

const DRAG_THRESHOLD_PX = 4;

interface DragState {
  pointerId: number;
  startX: number;
  startY: number;
  dragging: boolean;
}

const stageRef = ref<HTMLElement | null>(null);
const canvasRef = ref<HTMLCanvasElement | null>(null);
const frame = ref<MaterialFrameDescriptor | null>(null);
const maskError = ref<string | null>(null);
const frameError = ref<string | null>(null);
const playbackFault = ref<string | null>(null);
const actionError = ref<string | null>(null);
const petError = computed(() => maskError.value ?? playbackFault.value ?? frameError.value ?? actionError.value);

let displayContext: CanvasRenderingContext2D | null = null;
let displaySize: DisplaySize = { width: 1, height: 1 };
let sourceFrame: RgbaFrame | null = null;
let sourceCanvas: HTMLCanvasElement | null = null;
let placement: FramePlacement = { offsetX: 0, offsetY: 0, width: 1, height: 1 };
let currentTriple: MaterialFrameTriple | null = null;
let resizeObserver: ResizeObserver | null = null;
let dprCleanup: (() => void) | null = null;
let layoutTimer: ReturnType<typeof setTimeout> | null = null;
let appliedLayoutKey = '';
let lastPresentationSequence = 0;
let dragState: DragState | null = null;
let disposed = false;
let unlisteners: Unlisten[] = [];
const maskQueue = createMaskQueue();
/**
 * 宿主帧描述是唯一时间来源；本组件不持有计时器、不按 GIF/CSS 自行推进。
 * 取帧门保证最多一个在途请求与一个最新待处理描述。
 */
const frameGate = new PetFrameRequestGate({
  request: (descriptor) => requestMaterialPixels(materialFrameTriple(descriptor)),
  onFrame: (descriptor, payload) => applyPackageFrame(descriptor, payload),
  onError: () => {
    if (!disposed) frameError.value = '未能取得当前素材帧，请稍后重试或重新选择动作。';
  },
});

function createMaskQueue(): MaskSyncQueue {
  return new MaskSyncQueue({
    dispatch: setHitMask,
    onSettled: (_mask, result) => {
      if (disposed || result.superseded) return;
      if (result.ok) {
        maskError.value = null;
        return;
      }
      // 安装失败只保留固定安全提示，不显示宿主原始错误。
      maskError.value = '宠物命中区域安装失败，请重启 Moe Buddy。';
    },
  });
}

function currentLayoutKey(): string {
  const width = Math.max(1, Math.round(window.innerWidth));
  const height = Math.max(1, Math.round(window.innerHeight));
  const dpr = Math.max(1, window.devicePixelRatio || 1);
  return `${width}x${height}@${dpr}`;
}

/** 显示 backing 宽高各至多 1024；窗口仍由 CSS 铺满，不按每个帧/DPI 复制画布。 */
function ensureDisplayCanvas(): boolean {
  const canvas = canvasRef.value;
  if (canvas === null) return false;
  let next: DisplaySize;
  try {
    next = displayBackingSize(window.innerWidth, window.innerHeight, window.devicePixelRatio);
  } catch {
    frameError.value = '宠物显示尺寸无效。';
    return false;
  }
  displaySize = next;
  if (canvas.width !== next.width || canvas.height !== next.height) {
    canvas.width = next.width;
    canvas.height = next.height;
    displayContext = null;
  }
  if (displayContext === null) {
    displayContext = canvas.getContext('2d');
    if (displayContext === null) {
      maskError.value = '宠物画布初始化失败。';
      return false;
    }
  }
  return true;
}

function releaseSource(): void {
  if (sourceCanvas !== null) {
    sourceCanvas.width = 0;
    sourceCanvas.height = 0;
  }
  sourceCanvas = null;
  sourceFrame = null;
  currentTriple = null;
}

function clearCanvas(): void {
  const canvas = canvasRef.value;
  if (canvas !== null) {
    canvas.width = 0;
    canvas.height = 0;
  }
  displayContext = null;
  appliedLayoutKey = '';
  releaseSource();
}

function drawSource(): void {
  if (displayContext === null || sourceCanvas === null) return;
  const canvas = displayContext.canvas;
  try {
    displayContext.clearRect(0, 0, canvas.width, canvas.height);
    displayContext.drawImage(
      sourceCanvas,
      placement.offsetX,
      placement.offsetY,
      placement.width,
      placement.height,
    );
    frameError.value = null;
  } catch {
    frameError.value = '宠物帧绘制失败。';
  }
}

function canvasForRgba(rgba: RgbaFrame): HTMLCanvasElement | null {
  const canvas = document.createElement('canvas');
  canvas.width = rgba.width;
  canvas.height = rgba.height;
  const context = canvas.getContext('2d');
  if (context === null) {
    canvas.width = 0;
    canvas.height = 0;
    return null;
  }
  try {
    context.putImageData(new ImageData(rgba.data, rgba.width, rgba.height), 0, 0);
  } catch {
    // 失败路径同样清空 backing，避免临时 Canvas 在替换失败后继续占预算。
    canvas.width = 0;
    canvas.height = 0;
    return null;
  }
  return canvas;
}

function publishMask(): void {
  if (disposed) return;
  if (sourceFrame === null || currentTriple === null || displayContext === null) return;
  let grid;
  try {
    grid = buildHitMask(sourceFrame, undefined, {
      width: displaySize.width,
      height: displaySize.height,
      placement,
    });
  } catch {
    maskError.value = '宠物命中区域生成失败。';
    return;
  }
  maskQueue.enqueue(attachMaskFrame(grid, currentTriple));
}

function publishEmptyMask(descriptor: MaterialFrameDescriptor): void {
  if (disposed) return;
  maskQueue.enqueue(attachMaskFrame(emptyHitMaskGrid(), materialFrameTriple(descriptor)));
}

function renderBuiltin(descriptor: MaterialFrameDescriptor): void {
  if (descriptor.frameIndex === null) return;
  if (!ensureDisplayCanvas()) return;
  /*
   * builtin 帧描述固定 320×320；源 RGBA/源 Canvas 必须保持该描述尺寸，只由显示
   * Canvas 等比放大。若按显示 backing 生成源帧，320×320@DPR4 会在替换瞬间同时
   * 持有新旧 1024×1024 源与显示 Canvas，峰值超过 16MiB 呈现预算。
   */
  const raster = renderCatFrame(descriptor.width, descriptor.height, descriptor.frameIndex);
  const source = rgbaFrameFromBuiltin(raster);
  const canvas = canvasForRgba(source);
  if (canvas === null) {
    frameError.value = '内置帧缓存初始化失败。';
    return;
  }
  releaseSource();
  sourceFrame = source;
  sourceCanvas = canvas;
  placement = framePlacement(source.width, source.height, displaySize.width, displaySize.height);
  currentTriple = materialFrameTriple(descriptor);
  drawSource();
  publishMask();
}

function applyPackageFrame(
  descriptor: MaterialFrameDescriptor,
  payload: ArrayBuffer | Uint8Array,
): void {
  if (disposed) return;
  const active = frame.value;
  if (
    active === null ||
    active.source !== 'package' ||
    !sameMaterialTriple(active, materialFrameTriple(descriptor))
  ) {
    return;
  }
  const rgba = rgbaFrameFromBinary(payload, descriptor.width, descriptor.height);
  if (rgba === null) {
    frameError.value = '桌面宿主返回的素材帧尺寸或字节数不匹配。';
    return;
  }
  if (!ensureDisplayCanvas()) return;
  const canvas = canvasForRgba(rgba);
  if (canvas === null) {
    frameError.value = '素材帧缓存初始化失败。';
    return;
  }
  releaseSource();
  sourceFrame = rgba;
  sourceCanvas = canvas;
  placement = framePlacement(rgba.width, rgba.height, displaySize.width, displaySize.height);
  currentTriple = materialFrameTriple(descriptor);
  drawSource();
  publishMask();
}

function handleFrame(descriptor: MaterialFrameDescriptor | null): void {
  if (disposed) return;
  if (descriptor === null) {
    frameError.value = '收到无法识别的帧状态。';
    return;
  }
  // 全局展示序号单调；迟到、重播或旧查询不得回填画面。
  if (descriptor.presentationSequence <= lastPresentationSequence) return;
  const previous = frame.value;
  lastPresentationSequence = descriptor.presentationSequence;
  frame.value = descriptor;
  playbackFault.value = descriptor.faulted ? '动画播放故障，已停止播放。' : null;
  if (descriptor.source === 'empty') {
    frameGate.invalidate();
    clearCanvas();
    frameError.value = null;
    publishEmptyMask(descriptor);
    return;
  }
  if (descriptor.source === 'builtin') {
    // 切回 builtin 同样使正式包在途回包失效。
    frameGate.invalidate();
    renderBuiltin(descriptor);
    return;
  }
  // 新装包或新播放代际：旧包的 Canvas/像素引用与在途取帧立即失效，不能先显示旧帧。
  const switched =
    previous === null ||
    previous.source !== 'package' ||
    previous.resourceGeneration !== descriptor.resourceGeneration ||
    previous.playbackGeneration !== descriptor.playbackGeneration;
  if (switched) {
    frameGate.invalidate();
    clearCanvas();
  }
  frameGate.offer(descriptor);
}

async function startFrameProtocol(): Promise<void> {
  try {
    const unlistenFrame = await listenFrameEvents(handleFrame);
    if (disposed) {
      await releaseListeners([unlistenFrame]);
      return;
    }
    unlisteners.push(unlistenFrame);
  } catch {
    frameError.value = '无法监听宠物帧事件。';
  }

  try {
    const initial = await requestFrame();
    if (disposed) return;
    handleFrame(initial);
  } catch {
    if (!disposed) frameError.value ??= '未取得初始帧，暂显示缓存第 1 帧。';
  }
}

function handleHitMaskError(message: string | null): void {
  if (disposed || message === null) return;
  maskError.value = message;
}

async function startHitMaskErrorProtocol(): Promise<void> {
  try {
    const unlisten = await listenHitMaskErrorEvents(handleHitMaskError);
    if (disposed) {
      await releaseListeners([unlisten]);
      return;
    }
    unlisteners.push(unlisten);
  } catch {
    // 固定错误通道不可用时，掩码安装失败仍由 setHitMask 的调用路径反馈。
  }
}

function watchDpr(): void {
  dprCleanup?.();
  dprCleanup = null;
  if (disposed) return;
  const dpr = Math.max(1, window.devicePixelRatio || 1);
  const media = window.matchMedia(`(resolution: ${dpr}dppx)`);
  const onChange = (): void => scheduleLayoutSync();
  media.addEventListener('change', onChange, { once: true });
  dprCleanup = () => media.removeEventListener('change', onChange);
}

function syncLayout(): void {
  if (disposed) return;
  const key = currentLayoutKey();
  if (key === appliedLayoutKey) return;
  const active = frame.value;
  if (active === null || active.source === 'empty') {
    appliedLayoutKey = key;
    return;
  }
  if (!ensureDisplayCanvas()) return;
  appliedLayoutKey = key;
  if (active.source === 'builtin') {
    renderBuiltin(active);
  } else if (sourceFrame !== null) {
    placement = framePlacement(sourceFrame.width, sourceFrame.height, displaySize.width, displaySize.height);
    drawSource();
    publishMask();
  }
  watchDpr();
}

function scheduleLayoutSync(): void {
  if (disposed || layoutTimer !== null) return;
  layoutTimer = setTimeout(() => {
    layoutTimer = null;
    syncLayout();
  }, 80);
}

async function openChat(): Promise<void> {
  try {
    await invokeWindowCommand('show_chat');
  } catch {
    actionError.value = '打开聊天失败，请从主窗口操作。';
  }
}

function onPointerDown(event: PointerEvent): void {
  if (event.button !== 0) return;
  event.preventDefault();
  dragState = {
    pointerId: event.pointerId,
    startX: event.screenX,
    startY: event.screenY,
    dragging: false,
  };
  const stage = stageRef.value;
  if (stage !== null) {
    stage.focus();
    try {
      stage.setPointerCapture(event.pointerId);
    } catch {
      // 某些合成事件没有可捕获的指针；不影响点击/拖动主流程。
    }
  }
}

function onPointerMove(event: PointerEvent): void {
  if (dragState === null || event.pointerId !== dragState.pointerId || dragState.dragging) return;
  const dx = event.screenX - dragState.startX;
  const dy = event.screenY - dragState.startY;
  if (Math.hypot(dx, dy) < DRAG_THRESHOLD_PX) return;
  dragState.dragging = true;
  event.preventDefault();
  void invokeWindowCommand('start_dragging').catch(() => {
    actionError.value = '拖动宠物失败。';
  });
}

function releaseCapture(pointerId: number): void {
  const stage = stageRef.value;
  if (stage === null) return;
  try {
    if (stage.hasPointerCapture(pointerId)) stage.releasePointerCapture(pointerId);
  } catch {
    // 指针已释放时无需处理。
  }
}

function onPointerUp(event: PointerEvent): void {
  if (dragState === null || event.pointerId !== dragState.pointerId) return;
  const shouldOpenChat = !dragState.dragging;
  dragState = null;
  releaseCapture(event.pointerId);
  if (shouldOpenChat) void openChat();
}

function onPointerCancel(event: PointerEvent): void {
  if (dragState === null || event.pointerId !== dragState.pointerId) return;
  dragState = null;
  releaseCapture(event.pointerId);
}

function onKeydown(event: KeyboardEvent): void {
  if (event.key !== 'Enter' && event.key !== ' ') return;
  event.preventDefault();
  void openChat();
}

const accessibleFrameText = computed(() => {
  const active = frame.value;
  if (active === null || active.source === 'empty' || active.frameIndex === null) return '宠物暂无画面';
  return `第 ${active.frameIndex + 1} 帧`;
});

onMounted(() => {
  const stage = stageRef.value;
  if (stage !== null && typeof ResizeObserver !== 'undefined') {
    resizeObserver = new ResizeObserver(() => scheduleLayoutSync());
    resizeObserver.observe(stage);
  }
  void startFrameProtocol();
  void startHitMaskErrorProtocol();
});

onBeforeUnmount(() => {
  disposed = true;
  frameGate.dispose();
  maskQueue.cancel();
  clearCanvas();
  const attached = unlisteners;
  unlisteners = [];
  void releaseListeners(attached).then((outcome) => {
    if (!outcome.ok) document.documentElement.dataset.listenerRelease = 'failed';
  });
  resizeObserver?.disconnect();
  resizeObserver = null;
  dprCleanup?.();
  dprCleanup = null;
  if (layoutTimer !== null) {
    clearTimeout(layoutTimer);
    layoutTimer = null;
  }
  frame.value = null;
});
</script>

<template>
  <div
    ref="stageRef"
    class="pet-stage"
    role="button"
    tabindex="0"
    aria-label="Moe Buddy 宠物：点击打开聊天，拖动移动位置"
    @pointerdown="onPointerDown"
    @pointermove="onPointerMove"
    @pointerup="onPointerUp"
    @pointercancel="onPointerCancel"
    @keydown="onKeydown"
  >
    <canvas ref="canvasRef" class="pet-canvas" aria-hidden="true" />
    <div class="sr-only" role="status" aria-live="polite">
      {{ playbackFault ? '动画已故障' : frame?.paused ? '宠物已暂停' : accessibleFrameText }}
    </div>
    <p v-if="petError" class="pet-error" role="status" :title="petError">{{ petError }}</p>
  </div>
</template>

<style scoped>
.pet-stage {
  position: fixed;
  inset: 0;
  background: transparent;
  cursor: grab;
  outline: none;
  touch-action: none;
  user-select: none;
  -webkit-user-select: none;
}

.pet-stage:active {
  cursor: grabbing;
}

.pet-canvas {
  display: block;
  width: 100%;
  height: 100%;
  background: transparent;
}

/*
 * 宿主用 canvas 的 alpha 轮廓裁剪宠物窗口；错误条必须落在猫身不透明区域内，
 * 否则掩码安装失败等提示会被窗口区域直接裁掉。这里锚定在头部中心（猫身坐标约 0.42）。
 */
.pet-error {
  position: absolute;
  top: calc(50% - 8vmin);
  left: 50%;
  z-index: 1;
  max-width: 50vmin;
  margin: 0;
  padding: 3px 8px;
  overflow: hidden;
  color: #7a3d32;
  font-size: clamp(9px, 3.2vmin, 12px);
  line-height: 1.4;
  text-overflow: ellipsis;
  white-space: nowrap;
  pointer-events: none;
  background: rgba(247, 235, 230, 0.92);
  border-radius: 999px;
  transform: translate(-50%, -50%);
}
</style>
