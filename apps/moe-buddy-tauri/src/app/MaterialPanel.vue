<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue';
import { releaseListeners, type Unlisten } from '../core/listeners';
import {
  MATERIAL_LIMITS,
  MATERIAL_PACK_PIXEL_BUDGET_BYTES,
  RevisionOrderedMaterialStates,
  builtinMaterialCommand,
  loadMaterialCommand,
  materialDropIntent,
  materialDropWithinArea,
  materialRecoveryLocked,
  materialStateErrorText,
  parseMaterialState,
  pauseMaterialCommand,
  playMaterialCommand,
  resumeMaterialCommand,
  stopMaterialCommand,
  unloadMaterialCommand,
  type MaterialCommand,
  type MaterialState,
  type MaterialStatus,
} from '../core/material';
import {
  dispatchMaterialCommand,
  listenDragDropEvent,
  listenMaterialEvents,
  requestMaterialState,
  type DragDropSignal,
} from '../platform/tauri';
import type { BuddyExpectation } from '../core/types';

const props = defineProps<{
  /** 发起时捕获的完整 B context；面板不读取“当前伙伴”或 B 的其他状态。 */
  context: BuddyExpectation;
  /** 连接、撤权、关闭确认等外部阻断；为空才允许发起素材命令。 */
  blockedReason: string | null;
}>();

const STATUS_LABELS: Readonly<Record<MaterialStatus, string>> = {
  playing: '播放中',
  paused: '已暂停',
  stopped: '已停止',
  faulted: '播放故障',
};

const material = ref<MaterialState | null>(null);
const selectedAction = ref('');
const notice = ref<string | null>(null);
const dragActive = ref(false);
const commandBusy = ref(false);
const busyContext = ref<string | null>(null);
const dropAreaRef = ref<HTMLElement | null>(null);
const stateOrder = new RevisionOrderedMaterialStates();
let disposed = false;
let interactionGeneration = 0;
let commandSequence = 0;
let activeCommandToken: number | null = null;
let unlisteners: Unlisten[] = [];

function contextKey(context: BuddyExpectation): string {
  return `${context.expectedBuddyId}\u0000${context.expectedBuddiesRevision}`;
}

const currentContextKey = computed(() => contextKey(props.context));
/** busy 只属于发起命令的 context；伙伴切换后旧命令不能继续占住新伙伴的按钮。 */
const commandBusyForContext = computed(
  () => commandBusy.value && busyContext.value === currentContextKey.value,
);

const state = computed(() => material.value);
const loading = computed(() => state.value?.loading === true);
const actions = computed(() => state.value?.actions ?? []);
const statusText = computed(() => (state.value === null ? '未知' : STATUS_LABELS[state.value.status]));
/**
 * 命中区域清除故障锁只由宿主固定错误码识别；锁定时素材宿主仍会拒绝非恢复命令，
 * 前端不得发出播放、加载、builtin、暂停或继续，避免成功回执与冻结画面矛盾。
 * 成功只读刷新若仍是该故障投影不会解锁；只有更高 revision 的实际恢复投影才恢复。
 */
const recoveryLocked = computed(() => materialRecoveryLocked(state.value));
const RECOVERY_LOCKED_MESSAGE = '素材命中区域尚未确认清除，只能停止、卸载或只读刷新。';
/** 恢复动作：停止与卸载用于确认清除旧命中区域，其余素材命令在故障锁下不得派发。 */
const recoverySafeCommands: ReadonlySet<MaterialCommand['type']> = new Set(['stop', 'unload']);
const packText = computed(() => {
  const current = state.value;
  if (current === null) return '尚未取得素材状态';
  if (current.pack !== null) {
    return `${current.pack.label} · ${current.pack.id} v${current.pack.version}`;
  }
  return current.source === 'builtin' ? '内置测试猫' : '未装素材包';
});
const decodedText = computed(() => {
  const current = state.value;
  if (current === null) return '';
  const kib = Math.floor(current.decodedBytes / 1024);
  return `已解码 ${kib} KiB · ${current.frameCount} 帧`;
});
const packBudgetMiB = MATERIAL_PACK_PIXEL_BUDGET_BYTES / (1024 * 1024);
const stateError = computed(() => materialStateErrorText(material.value));
const blockedReason = computed(() => props.blockedReason);
const canOperate = computed(() => blockedReason.value === null && state.value !== null);
const canLoad = computed(
  () =>
    canOperate.value &&
    !loading.value &&
    !commandBusyForContext.value &&
    !recoveryLocked.value,
);
const canPlay = computed(
  () =>
    canOperate.value &&
    !loading.value &&
    !commandBusyForContext.value &&
    !recoveryLocked.value &&
    state.value?.source !== 'empty' &&
    selectedAction.value.length > 0,
);
const canPause = computed(
  () =>
    canOperate.value &&
    !commandBusyForContext.value &&
    !recoveryLocked.value &&
    state.value?.status === 'playing',
);
const canResume = computed(
  () =>
    canOperate.value &&
    !commandBusyForContext.value &&
    !recoveryLocked.value &&
    state.value?.status === 'paused',
);
/**
 * 加载中的候选可能来自 empty/stopped；此时 stop/unload 必须仍可发出，才能取消
 * 加载。命中区域清除故障锁（含 empty+故障）仍保留显式停止/卸载入口，以确认
 * 尚未清除的原生命中区域。
 */
const canStop = computed(
  () =>
    canOperate.value &&
    !commandBusyForContext.value &&
    (recoveryLocked.value ||
      loading.value ||
      state.value?.status === 'faulted' ||
      (state.value !== null &&
        state.value.source !== 'empty' &&
        state.value.status !== 'stopped')),
);
const canUnload = computed(
  () =>
    canOperate.value &&
    !commandBusyForContext.value &&
    (recoveryLocked.value ||
      loading.value ||
      state.value?.source !== 'empty' ||
      state.value?.status === 'faulted'),
);
const canBuiltin = computed(
  () =>
    canOperate.value &&
    !loading.value &&
    !commandBusyForContext.value &&
    !recoveryLocked.value &&
    state.value?.source !== 'builtin',
);

function applyState(candidate: MaterialState | null): boolean {
  if (candidate === null) return false;
  if (!stateOrder.apply(candidate)) return false;
  material.value = candidate;
  if (candidate.actionId !== null) {
    selectedAction.value = candidate.actionId;
  } else if (!candidate.actions.some((action) => action.id === selectedAction.value)) {
    selectedAction.value = candidate.actions[0]?.id ?? '';
  }
  return true;
}

async function refreshState(): Promise<void> {
  if (disposed) return;
  try {
    const current = await requestMaterialState();
    if (!disposed) {
      applyState(current);
      // 只读重读成功后，未确认提示可由新投影替换；不据此自动重发命令。
      notice.value = null;
    }
  } catch {
    if (!disposed) notice.value = '无法读取素材状态，请从桌面宿主重试。';
  }
}

async function send(command: MaterialCommand): Promise<boolean> {
  if (disposed) return false;
  if (blockedReason.value !== null) {
    notice.value = blockedReason.value;
    return false;
  }
  // 故障锁下即使按钮禁用或处理函数被直接调用，也不得把非恢复命令推进命令令牌流程。
  if (recoveryLocked.value && !recoverySafeCommands.has(command.type)) {
    notice.value = RECOVERY_LOCKED_MESSAGE;
    return false;
  }
  if (commandBusyForContext.value) {
    notice.value = '素材命令正在处理；请等待本次结果，不会自动重发。';
    return false;
  }
  // 发起时冻结完整 B context；迟到回执按该 context 与本次命令令牌收口。
  const context = { ...props.context };
  const contextAtStart = contextKey(context);
  const generation = interactionGeneration;
  const token = ++commandSequence;
  activeCommandToken = token;
  busyContext.value = contextAtStart;
  commandBusy.value = true;
  notice.value = null;
  try {
    const reply = await dispatchMaterialCommand(command, context);
    if (disposed || generation !== interactionGeneration) return false;
    // 素材投影是进程级只读状态，始终按 revision 收下；迟到提示则按 context 隔离。
    const wasCurrent = (stateOrder.current?.revision ?? 0) <= reply.state.revision;
    const applied = applyState(reply.state);
    if (
      !reply.ok &&
      (applied || wasCurrent) &&
      activeCommandToken === token &&
      contextAtStart === currentContextKey.value
    ) {
      notice.value = reply.error?.message ?? '桌面宿主拒绝了本次素材命令。';
    }
    return reply.ok;
  } catch {
    if (
      !disposed &&
      generation === interactionGeneration &&
      activeCommandToken === token &&
      contextAtStart === currentContextKey.value
    ) {
      notice.value = '素材命令结果未确认；请刷新素材状态后再操作（不会自动重发）。';
    }
    return false;
  } finally {
    if (activeCommandToken === token) {
      activeCommandToken = null;
      busyContext.value = null;
      commandBusy.value = false;
    }
  }
}

async function loadPath(path: string): Promise<boolean> {
  const current = material.value;
  if (current === null) {
    notice.value = '尚未取得素材状态，暂不能加载素材包。';
    return false;
  }
  if (loading.value) {
    notice.value = '素材正在加载；请等待当前加载结束或先停止。';
    return false;
  }
  let command: MaterialCommand;
  try {
    command = loadMaterialCommand(path, current);
  } catch {
    notice.value = '素材路径无效，未发起加载。';
    return false;
  }
  return send(command);
}

function handleDropPaths(paths: unknown): Promise<boolean> {
  const intent = materialDropIntent(paths);
  if (!intent.ok || intent.path === undefined) {
    notice.value = intent.message ?? '素材路径无效。';
    return Promise.resolve(false);
  }
  return loadPath(intent.path);
}

function dropPositionInside(position: DragDropSignal['position']): boolean {
  const element = dropAreaRef.value;
  if (element === null || position === null) return false;
  let rect: DOMRect;
  try {
    rect = element.getBoundingClientRect();
  } catch {
    return false;
  }
  return materialDropWithinArea(position, rect, window.devicePixelRatio);
}

function handleDragSignal(signal: DragDropSignal): void {
  if (signal.type === 'leave') {
    dragActive.value = false;
    return;
  }
  const inside = dropPositionInside(signal.position);
  if (!inside) {
    dragActive.value = false;
    if (signal.type === 'drop' && !disposed) {
      notice.value = '请把素材包拖到上方素材区域内。';
    }
    return;
  }
  dragActive.value = signal.type === 'enter' || signal.type === 'over';
  if (signal.type !== 'drop' || disposed) return;
  dragActive.value = false;
  if (blockedReason.value !== null) {
    notice.value = blockedReason.value;
    return;
  }
  void handleDropPaths(signal.paths);
}

function handleMaterialEvent(candidate: MaterialState | null): void {
  if (disposed || candidate === null) return;
  // 事件适配层已解析；这里再次严格解析，防止阻塞窗口、旧协议或缺字段投影被当成状态。
  const parsed = parseMaterialState(candidate);
  if (parsed === null) return;
  applyState(parsed);
}

async function attachProtocols(): Promise<void> {
  try {
    const unlistenMaterial = await listenMaterialEvents(handleMaterialEvent);
    if (disposed) {
      await releaseListeners([unlistenMaterial]);
      return;
    }
    unlisteners.push(unlistenMaterial);
  } catch {
    if (!disposed) notice.value = '无法监听素材状态事件。';
  }
  try {
    const unlistenDrag = await listenDragDropEvent(handleDragSignal);
    if (disposed) {
      await releaseListeners([unlistenDrag]);
      return;
    }
    unlisteners.push(unlistenDrag);
  } catch {
    if (!disposed) notice.value = '无法接收素材拖放事件。';
  }
  await refreshState();
}

async function playSelected(): Promise<void> {
  const current = material.value;
  if (current === null || selectedAction.value.length === 0) return;
  let command: MaterialCommand;
  try {
    command = playMaterialCommand(selectedAction.value, current);
  } catch {
    notice.value = '动作无效，未发起播放。';
    return;
  }
  await send(command);
}

async function pausePlayback(): Promise<void> {
  if (material.value === null) return;
  await send(pauseMaterialCommand(material.value));
}

async function resumePlayback(): Promise<void> {
  if (material.value === null) return;
  await send(resumeMaterialCommand(material.value));
}

async function stopPlayback(): Promise<void> {
  if (material.value === null) return;
  await send(stopMaterialCommand(material.value));
}

async function unloadMaterial(): Promise<void> {
  if (material.value === null) return;
  await send(unloadMaterialCommand(material.value));
}

async function restoreBuiltin(): Promise<void> {
  if (material.value === null) return;
  await send(builtinMaterialCommand(material.value));
}

onMounted(() => {
  void attachProtocols();
});

onBeforeUnmount(() => {
  disposed = true;
  interactionGeneration += 1;
  stateOrder.reset();
  material.value = null;
  selectedAction.value = '';
  dragActive.value = false;
  const attached = unlisteners;
  unlisteners = [];
  void releaseListeners(attached).then((outcome) => {
    if (!outcome.ok) document.documentElement.dataset.materialListenerRelease = 'failed';
  });
});
</script>

<template>
  <section class="material-panel" aria-label="素材包与播放">
    <div
      ref="dropAreaRef"
      class="material-drop"
      :class="{ active: dragActive, disabled: !canLoad }"
      role="group"
      aria-label="素材拖放区域"
      :title="blockedReason ?? '把素材包目录或其中的 manifest.json 拖到这里'"
    >
      <p class="material-drop-title">拖入素材包目录或 manifest.json</p>
      <p class="material-drop-hint">
        支持 8 位非交错 PNG 序列帧与 GIF；单包最多 {{ MATERIAL_LIMITS.packageMaxFrames }}
        帧、解码 {{ packBudgetMiB }} MiB，仅从本地磁盘只读读取。
      </p>
    </div>

    <div class="material-readout" role="status" aria-live="polite">
      <p class="material-pack">{{ packText }}</p>
      <p class="muted">
        状态：{{ statusText }}<span v-if="loading"> · 加载中</span>
        <span v-if="decodedText"> · {{ decodedText }}</span>
      </p>
      <p v-if="blockedReason" class="material-warn">{{ blockedReason }}</p>
      <p v-if="stateError" class="material-warn">{{ stateError }}</p>
      <p v-if="notice" class="material-warn">{{ notice }}</p>
    </div>

    <div class="material-controls">
      <label class="material-action">
        动作
        <select v-model="selectedAction" :disabled="!canOperate || commandBusyForContext || actions.length === 0">
          <option v-if="actions.length === 0" value="">无可用动作</option>
          <option v-for="action in actions" :key="action.id" :value="action.id">{{ action.label }}</option>
        </select>
      </label>
      <button type="button" :disabled="!canPlay" @click="playSelected">播放</button>
      <button type="button" :disabled="!canPause" @click="pausePlayback">暂停</button>
      <button type="button" :disabled="!canResume" @click="resumePlayback">继续</button>
      <button type="button" :disabled="!canStop" @click="stopPlayback">停止</button>
      <button type="button" :disabled="!canUnload" @click="unloadMaterial">卸载</button>
      <button type="button" :disabled="!canBuiltin" @click="restoreBuiltin">恢复测试猫</button>
      <button type="button" class="material-refresh" @click="refreshState">刷新状态（只读）</button>
    </div>
  </section>
</template>

<style scoped>
.material-panel {
  display: grid;
  gap: 8px;
  padding: 10px 12px;
  margin: 8px 0;
  background: rgba(246, 241, 229, 0.82);
  border: 1px dashed rgba(84, 121, 100, 0.45);
  border-radius: 10px;
}

.material-drop {
  padding: 10px 12px;
  text-align: center;
  border: 1px dashed rgba(84, 121, 100, 0.55);
  border-radius: 8px;
  background: rgba(255, 255, 255, 0.55);
}

.material-drop.active {
  border-color: #547964;
  background: rgba(214, 235, 220, 0.72);
}

.material-drop.disabled {
  opacity: 0.62;
}

.material-drop-title {
  margin: 0 0 4px;
  font-weight: 600;
  color: #2f473c;
}

.material-drop-hint {
  margin: 0;
  font-size: 12px;
  line-height: 1.5;
  color: #5e6f66;
}

.material-readout {
  display: grid;
  gap: 2px;
  font-size: 13px;
}

.material-pack {
  margin: 0;
  font-weight: 600;
  overflow-wrap: anywhere;
}

.material-readout .muted {
  margin: 0;
  color: #5e6f66;
}

.material-warn {
  margin: 0;
  color: #7a3d32;
  overflow-wrap: anywhere;
}

.material-controls {
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  align-items: end;
}

.material-action {
  display: grid;
  gap: 2px;
  font-size: 12px;
  color: #3e5349;
}

.material-action select {
  min-width: 8rem;
}
</style>
