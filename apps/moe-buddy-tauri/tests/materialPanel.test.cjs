const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');

// Execute the actual MaterialPanel <script setup> with controlled IPC completion.
// This is real component async logic (not a reimplementation), but not a DOM paint.
function component() {
  const sourcePath = path.join(__dirname, '../src/app/MaterialPanel.vue');
  const source = fs.readFileSync(sourcePath, 'utf8').match(/<script setup lang="ts">([\s\S]*?)<\/script>/)[1];
  const script = ts.transpileModule(source + `
    module.exports.probe = {
      material, selectedAction, notice, dragActive, commandBusy, commandBusyForContext,
      canLoad, canPlay, canPause, canResume, canStop, canUnload, canBuiltin,
      applyState, attachProtocols, handleDropPaths, handleDragSignal,
      playSelected, pausePlayback, resumePlayback, stopPlayback, unloadMaterial, restoreBuiltin,
      refreshState, dropAreaRef,
    };
  `, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;

  const contextHolder = vue.ref({ expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 7 });
  const blockedHolder = vue.ref(null);
  const pageHiddenHolder = vue.ref(false);
  let materialHandler = () => {};
  let dragHandler = () => {};
  const stateRequests = [];
  const commandCalls = [];
  const mounted = [];
  const beforeUnmount = [];

  const platform = {
    requestMaterialState: () =>
      new Promise((resolve, reject) => stateRequests.push({ resolve, reject })),
    dispatchMaterialCommand: (command, expectation) =>
      new Promise((resolve, reject) => commandCalls.push({ command, expectation, resolve, reject })),
    listenMaterialEvents: async (handler) => {
      materialHandler = handler;
      return () => {};
    },
    listenDragDropEvent: async (handler) => {
      dragHandler = handler;
      return () => {};
    },
  };

  const scope = vue.effectScope();
  const exported = { exports: {} };
  const context = {
    module: exported,
    exports: exported.exports,
    setTimeout,
    clearTimeout,
    window: { devicePixelRatio: 2 },
    document: { documentElement: { dataset: {} } },
    defineProps: () => ({
      get context() {
        return contextHolder.value;
      },
      get blockedReason() {
        return blockedHolder.value;
      },
      get pageHidden() {
        return pageHiddenHolder.value;
      },
    }),
    require(name) {
      if (name === 'vue') {
        return {
          ...vue,
          onMounted(callback) {
            mounted.push(callback);
          },
          onBeforeUnmount(callback) {
            beforeUnmount.push(callback);
          },
        };
      }
      if (name === '../platform/tauri') return platform;
      if (name.startsWith('../core/')) {
        return require(path.join(__dirname, '../.test-build', name.slice('../core/'.length) + '.js'));
      }
      throw new Error(`Unexpected component dependency: ${name}`);
    },
  };
  scope.run(() => vm.runInNewContext(script, context, { filename: sourcePath }));
  const api = exported.exports.probe;
  // CSS 区域 (0,0)-(200,100)，DPR=2 时物理区域为 (0,0)-(400,200)。
  api.dropAreaRef.value = {
    getBoundingClientRect: () => ({ left: 0, top: 0, width: 200, height: 100 }),
  };

  let unmounted = false;
  return {
    api,
    stateRequests,
    commandCalls,
    emitMaterial(state) {
      materialHandler(state);
    },
    emitDrag(signal) {
      dragHandler({ position: { x: 100, y: 50 }, ...signal });
    },
    setContext(next) {
      contextHolder.value = next;
    },
    setBlocked(reason) {
      blockedHolder.value = reason;
    },
    setPageHidden(value) {
      pageHiddenHolder.value = value;
    },
    async mount() {
      for (const callback of mounted) callback();
      await flush();
    },
    unmount() {
      if (unmounted) return;
      unmounted = true;
      for (const callback of beforeUnmount) callback();
      scope.stop();
    },
  };
}

const tick = () => new Promise((resolve) => setImmediate(resolve));
async function flush() {
  await tick();
  await tick();
}

/** Objects built inside the vm realm have a different prototype; compare plain data. */
function plain(value) {
  return JSON.parse(JSON.stringify(value));
}

function materialState(overrides = {}) {
  return {
    revision: 1,
    resourceGeneration: 1,
    playbackGeneration: 1,
    source: 'package',
    pack: { id: 'synthetic-seq', label: '合成序列', version: 1 },
    actions: [
      { id: 'idle', label: '待机' },
      { id: 'wave', label: '招手' },
    ],
    actionId: 'idle',
    status: 'playing',
    loading: false,
    decodedBytes: 4096,
    frameCount: 4,
    error: null,
    ...overrides,
  };
}

async function mountedWithState(state = materialState()) {
  const instance = component();
  await instance.mount();
  assert.equal(instance.stateRequests.length, 1, 'initial state read requested after listeners');
  instance.stateRequests[0].resolve(state);
  await flush();
  assert.equal(instance.api.material.value.revision, state.revision);
  return instance;
}

function reply(state, overrides = {}) {
  return { ok: true, state, error: null, ...overrides };
}

test('真实素材组件：初始查询与事件按 revision 排序，旧事件/旧查询不得覆盖新状态', async () => {
  const instance = await mountedWithState(materialState({ revision: 2, status: 'playing' }));
  try {
    instance.emitMaterial(materialState({ revision: 5, status: 'paused', actionId: 'wave' }));
    assert.equal(instance.api.material.value.revision, 5);
    assert.equal(instance.api.material.value.status, 'paused');
    instance.emitMaterial(materialState({ revision: 4, status: 'playing' }));
    assert.equal(instance.api.material.value.revision, 5);
    // 非法事件整份丢弃，不替换当前投影。
    instance.emitMaterial({ ...materialState({ revision: 6 }), path: 'C:/secret' });
    instance.emitMaterial(null);
    assert.equal(instance.api.material.value.revision, 5);
    assert.equal(instance.api.material.value.pack.id, 'synthetic-seq');
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：拖放首次事件捕获完整 B context，后续切换不替换本次 load 意图', async () => {
  const instance = await mountedWithState();
  try {
    instance.setContext({ expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 7 });
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\cat'] });
    await flush();
    assert.equal(instance.commandCalls.length, 1);
    assert.deepEqual(plain(instance.commandCalls[0].command), {
      type: 'load',
      path: 'C:\\packs\\cat',
      expectedResourceGeneration: 1,
      expectedPlaybackGeneration: 1,
    });
    assert.deepEqual(plain(instance.commandCalls[0].expectation), {
      expectedBuddyId: 'buddy-a',
      expectedBuddiesRevision: 7,
    });
    // 排队期间伙伴切换不得改写已捕获的 intent；回复只更新素材投影。
    instance.setContext({ expectedBuddyId: 'buddy-b', expectedBuddiesRevision: 8 });
    instance.commandCalls[0].resolve(
      reply(materialState({ revision: 2, loading: true, status: 'stopped' })),
    );
    await flush();
    assert.equal(instance.api.material.value.revision, 2);
    assert.equal(instance.api.material.value.loading, true);
    assert.equal(instance.api.material.value.pack.id, 'synthetic-seq');
    assert.equal(instance.commandCalls[0].expectation.expectedBuddyId, 'buddy-a');
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：坏包/拒绝失败保留旧包与动作，不把失败说成新包成功', async () => {
  const instance = await mountedWithState();
  try {
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\bad'] });
    await flush();
    assert.equal(instance.commandCalls.length, 1);
    instance.commandCalls[0].resolve({
      ok: false,
      state: materialState({
        revision: 2,
        status: 'faulted',
        error: { code: 'pack_rejected', message: '素材包校验失败。' },
      }),
      error: { code: 'pack_rejected', message: '素材包校验失败。', disposition: 'not_executed' },
    });
    await flush();
    assert.equal(instance.api.material.value.pack.id, 'synthetic-seq');
    assert.deepEqual(instance.api.material.value.actions.map((action) => action.id), ['idle', 'wave']);
    assert.equal(instance.api.notice.value, '素材包校验失败。');
    assert.equal(instance.api.material.value.status, 'faulted');
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：invoke 失败只报结果未确认，提供只读刷新且不自动重发', async () => {
  const instance = await mountedWithState();
  try {
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\bad'] });
    await flush();
    assert.equal(instance.commandCalls.length, 1);
    instance.commandCalls[0].reject(new Error('synthetic IPC loss'));
    await flush();
    assert.equal(instance.api.material.value.pack.id, 'synthetic-seq');
    assert.match(instance.api.notice.value, /未确认/);
    assert.match(instance.api.notice.value, /刷新/);
    assert.equal(instance.api.notice.value.includes('保持不变'), false);
    assert.equal(instance.commandCalls.length, 1, '失败后不得自动重发');
    assert.equal(instance.api.commandBusyForContext.value, false);

    // 只读刷新：新增状态查询，不另发素材命令。
    const refreshing = instance.api.refreshState();
    await flush();
    assert.equal(instance.stateRequests.length, 2);
    instance.stateRequests[1].resolve(materialState({ revision: 3, status: 'playing' }));
    await refreshing;
    assert.equal(instance.commandCalls.length, 1);
    assert.equal(instance.api.material.value.revision, 3);
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：loading 期间 load 背压，但从 empty/stopped 加载时 stop/unload 可取消', async () => {
  const instance = await mountedWithState(materialState({ status: 'stopped' }));
  try {
    instance.emitMaterial(
      materialState({ revision: 2, loading: true, status: 'stopped', source: 'package' }),
    );
    await flush();
    assert.equal(instance.api.canLoad.value, false);
    assert.equal(instance.api.canStop.value, true, '加载中的 stopped 资源仍可停止以取消');
    assert.equal(instance.api.canUnload.value, true, '加载中可由卸载取消');
    await instance.api.handleDropPaths(['C:\\packs\\other']);
    assert.equal(instance.api.notice.value.includes('正在加载'), true);
    assert.equal(instance.commandCalls.length, 0, 'loading 期间不得发起第二个 load');

    const stopping = instance.api.stopPlayback();
    await flush();
    assert.equal(instance.commandCalls.length, 1);
    assert.equal(instance.commandCalls[0].command.type, 'stop');
    instance.commandCalls[0].resolve(
      reply(materialState({ revision: 3, status: 'stopped', loading: false })),
    );
    await stopping;

    // empty 起始的加载同样允许 stop/unload 取消。
    instance.emitMaterial(
      materialState({
        revision: 4,
        source: 'empty',
        pack: null,
        actions: [],
        actionId: null,
        status: 'stopped',
        loading: true,
        decodedBytes: 0,
        frameCount: 0,
      }),
    );
    await flush();
    assert.equal(instance.api.canStop.value, true);
    assert.equal(instance.api.canUnload.value, true);
    const unloading = instance.api.unloadMaterial();
    await flush();
    assert.equal(instance.commandCalls.at(-1).command.type, 'unload');
    instance.commandCalls.at(-1).resolve(
      reply(materialState({
        revision: 5,
        source: 'empty',
        pack: null,
        actions: [],
        actionId: null,
        status: 'stopped',
        loading: false,
        decodedBytes: 0,
        frameCount: 0,
      })),
    );
    await unloading;

    // 非加载的 empty 没有可停止/卸载资源。
    assert.equal(instance.api.canStop.value, false);
    assert.equal(instance.api.canUnload.value, false);
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：命令提示与 busy 按发起 context 收口，迟到回复不串到新伙伴', async () => {
  const instance = await mountedWithState();
  try {
    instance.setContext({ expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 7 });
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\a'] });
    await flush();
    assert.equal(instance.commandCalls.length, 1);
    assert.equal(instance.commandCalls[0].expectation.expectedBuddyId, 'buddy-a');
    assert.equal(instance.api.commandBusyForContext.value, true);
    // 同 context 并发 drop 不派发第二条命令，busy 收口由本次命令令牌负责。
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\a2'] });
    await flush();
    assert.equal(instance.commandCalls.length, 1, '同 context 不得并发第二条命令');
    assert.match(instance.api.notice.value, /正在处理/);

    // 伙伴切换后旧命令不再占住新伙伴的 busy，也不显示旧 context 的迟到提示。
    instance.setContext({ expectedBuddyId: 'buddy-b', expectedBuddiesRevision: 8 });
    await flush();
    assert.equal(instance.api.commandBusyForContext.value, false);
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\b'] });
    await flush();
    assert.equal(instance.commandCalls.length, 2);
    assert.equal(instance.commandCalls[1].expectation.expectedBuddyId, 'buddy-b');
    assert.equal(instance.api.commandBusyForContext.value, true);

    instance.commandCalls[0].resolve({
      ok: false,
      state: materialState({ revision: 2 }),
      error: { code: 'stale', message: '旧伙伴命令已失效。', disposition: 'not_executed' },
    });
    await flush();
    assert.equal(instance.api.notice.value, null, '旧 context 的迟到提示不得串到新伙伴');
    assert.equal(instance.api.commandBusyForContext.value, true, '旧命令结束不得清掉新伙伴的 busy');
    // 素材投影本身是进程级只读状态，仍按 revision 收下。
    assert.equal(instance.api.material.value.revision, 2);

    instance.commandCalls[1].resolve(reply(materialState({ revision: 3 })));
    await flush();
    assert.equal(instance.api.commandBusyForContext.value, false);
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：拖放在素材区域外不加载，区域内/边界按 DPR 物理坐标判断', async () => {
  const instance = await mountedWithState();
  try {
    // CSS 区域 (0,0)-(200,100)，DPR=2；x=401 已在物理区域外。
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\outside'], position: { x: 401, y: 50 } });
    await flush();
    assert.equal(instance.commandCalls.length, 0, '区域外 drop 不得加载');
    assert.match(instance.api.notice.value, /素材区域/);
    assert.equal(instance.api.dragActive.value, false);

    instance.emitDrag({ type: 'enter', paths: ['C:\\packs\\inside'], position: { x: 400, y: 200 } });
    assert.equal(instance.api.dragActive.value, true, '物理区域右/下边界属于素材区域');
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\inside'], position: { x: 400, y: 200 } });
    await flush();
    assert.equal(instance.commandCalls.length, 1, '区域内 drop 正常发起一次加载');
    assert.equal(instance.commandCalls[0].command.path, 'C:\\packs\\inside');
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：多路径/UNC/相对路径不派发，固定提示不回显路径', async () => {
  const instance = await mountedWithState();
  try {
    instance.emitDrag({ type: 'drop', paths: ['C:\\a', 'C:\\b'] });
    await flush();
    assert.equal(instance.commandCalls.length, 0);
    assert.equal(instance.api.notice.value.includes('C:'), false);
    instance.emitDrag({ type: 'drop', paths: ['\\\\server\\share\\cat'] });
    await flush();
    assert.equal(instance.commandCalls.length, 0);
    assert.equal(instance.api.notice.value.includes('server'), false);
    instance.emitDrag({ type: 'drop', paths: ['cat\\manifest.json'] });
    await flush();
    assert.equal(instance.commandCalls.length, 0);
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：动作选择、暂停/继续/停止/卸载/恢复测试猫均按当前双代际发命令', async () => {
  const instance = await mountedWithState();
  try {
    assert.equal(instance.api.selectedAction.value, 'idle');
    instance.api.selectedAction.value = 'wave';
    const playing = instance.api.playSelected();
    await flush();
    assert.deepEqual(plain(instance.commandCalls[0].command), {
      type: 'play',
      actionId: 'wave',
      expectedResourceGeneration: 1,
      expectedPlaybackGeneration: 1,
    });
    instance.commandCalls[0].resolve(reply(materialState({ revision: 2, status: 'playing', actionId: 'wave' })));
    await playing;

    for (const [call, type] of [
      [() => instance.api.pausePlayback(), 'pause'],
      [() => instance.api.stopPlayback(), 'stop'],
      [() => instance.api.unloadMaterial(), 'unload'],
      [() => instance.api.restoreBuiltin(), 'builtin'],
    ]) {
      const pending = call();
      await flush();
      const record = instance.commandCalls.at(-1);
      assert.equal(record.command.type, type);
      assert.equal(record.command.expectedResourceGeneration, 1);
      assert.equal(record.command.expectedPlaybackGeneration, 1);
      record.resolve(reply(materialState({ revision: instance.api.material.value.revision + 1 })));
      await pending;
    }
    // 暂停后再继续使用同一冻结命令形状。
    instance.emitMaterial(materialState({ revision: 20, status: 'paused' }));
    const resuming = instance.api.resumePlayback();
    await flush();
    assert.equal(instance.commandCalls.at(-1).command.type, 'resume');
    instance.commandCalls.at(-1).resolve(reply(materialState({ revision: 21, status: 'playing' })));
    await resuming;
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：卸载/释放后迟到回复与事件不回填；不消费 buddyCommit 或管理稿', async () => {
  const snapshot = Object.freeze({
    activeBuddyId: 'buddy-a',
    buddiesRevision: 7,
    buddyCommit: Object.freeze({
      kind: 'configure_model',
      fromBuddyId: 'buddy-a',
      fromRevision: 1,
      toBuddyId: 'buddy-a',
      toRevision: 2,
      managementVersion: 3,
    }),
    managementDraft: Object.freeze({ version: 3, dirty: true, kind: 'general' }),
  });
  const before = JSON.stringify(snapshot);
  const instance = await mountedWithState();
  instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\cat'] });
  await flush();
  assert.equal(instance.commandCalls.length, 1);
  instance.unmount();
  instance.commandCalls[0].resolve(reply(materialState({ revision: 9 })));
  instance.emitMaterial(materialState({ revision: 10 }));
  await flush();
  assert.equal(instance.api.material.value, null);
  assert.equal(JSON.stringify(snapshot), before);
  assert.equal(snapshot.buddyCommit !== null, true);
  assert.equal(snapshot.managementDraft.dirty, true);
});

test('真实素材组件：builtin 必须带可操作 idle 动作；动作目录为空或 idle 不在目录的状态被生产解析拒绝', async () => {
  const instance = component();
  await instance.mount();
  try {
    const builtin = materialState({
      revision: 2,
      source: 'builtin',
      pack: null,
      decodedBytes: 0,
      frameCount: 4,
      actions: [{ id: 'idle', label: '待机' }],
      actionId: 'idle',
      status: 'playing',
      loading: false,
      error: null,
    });
    instance.stateRequests[0].resolve(builtin);
    await flush();
    assert.equal(instance.api.material.value.source, 'builtin');
    assert.equal(instance.api.selectedAction.value, 'idle');
    assert.equal(instance.api.canPlay.value, true, 'builtin idle must be playable from the real panel');
    assert.equal(instance.api.canBuiltin.value, false);

    // 初始宿主缺陷形状：actionId=idle 但 actions 为空；生产 parseMaterialState 拒绝，事件不得覆盖。
    instance.emitMaterial({ ...builtin, revision: 3, actions: [], actionId: 'idle' });
    await flush();
    assert.equal(instance.api.material.value.revision, 2, 'invalid builtin projection must be dropped');
    assert.equal(instance.api.material.value.actions.length, 1);
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：宿主阻断时明确提示且不派发，不读取路径作为展示', async () => {
  const instance = await mountedWithState();
  try {
    instance.setBlocked('关闭/退出确认处理中，暂不能变更素材。');
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\cat'] });
    await flush();
    assert.equal(instance.commandCalls.length, 0);
    assert.equal(instance.api.notice.value, '关闭/退出确认处理中，暂不能变更素材。');
    assert.equal(instance.api.notice.value.includes('C:'), false);
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：所属页面隐藏时忽略全部拖放信号，恢复可见后才接受投放', async () => {
  const instance = await mountedWithState();
  try {
    instance.setPageHidden(true);
    instance.emitDrag({ type: 'enter', paths: ['C:\\packs\\hidden'] });
    assert.equal(instance.api.dragActive.value, false, '隐藏页不得进入拖拽高亮');
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\hidden'] });
    await flush();
    assert.equal(instance.commandCalls.length, 0, '隐藏页不接受拖放加载');
    assert.equal(instance.api.notice.value, null, '隐藏页的拖放不产生任何提示');

    instance.setPageHidden(false);
    instance.emitDrag({ type: 'enter', paths: ['C:\\packs\\visible'] });
    assert.equal(instance.api.dragActive.value, true, '恢复可见后拖拽高亮恢复');
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\visible'] });
    await flush();
    assert.equal(instance.commandCalls.length, 1, '恢复可见后正常接受一次投放');
    assert.equal(instance.commandCalls[0].command.path, 'C:\\packs\\visible');
  } finally {
    instance.unmount();
  }
});

test('卸载后命中区域清除故障仍可通过停止按钮显式恢复', async () => {
  const instance = await mountedWithState(materialState({
    source: 'empty', pack: null, actions: [], actionId: null,
    frameCount: 0, decodedBytes: 0, status: 'faulted',
    error: { code: 'material_hit_mask_clear_failed', message: '无法确认旧命中区域已清除' },
  }));
  try {
    assert.equal(instance.api.canStop.value, true);
    const pending = instance.api.stopPlayback();
    assert.equal(instance.commandCalls.length, 1);
    const call = instance.commandCalls[0];
    assert.equal(call.command.type, 'stop');
    call.resolve({ ok: true, state: { ...plain(instance.api.material.value), revision: 2, status: 'stopped', error: null }, error: null });
    await pending;
    assert.equal(instance.api.canStop.value, false);
  } finally {
    instance.unmount();
  }
});

/** 宿主固定命中区域清除故障码；文案不参与判定。 */
const HIT_MASK_FAULT_CODE = 'material_hit_mask_clear_failed';

function hitMaskFaultState(overrides = {}) {
  return materialState({
    status: 'faulted',
    error: { code: HIT_MASK_FAULT_CODE, message: '无法确认旧命中区域已清除，请显式重试清除' },
    ...overrides,
  });
}

function emptyState(overrides = {}) {
  return materialState({
    source: 'empty',
    pack: null,
    actions: [],
    actionId: null,
    status: 'stopped',
    loading: false,
    decodedBytes: 0,
    frameCount: 0,
    error: null,
    ...overrides,
  });
}

test('真实素材组件：命中区域清除故障锁定全部非恢复入口，只读刷新与低修订恢复不解锁', async () => {
  const instance = await mountedWithState(hitMaskFaultState({ revision: 10 }));
  try {
    // package 故障锁：非恢复按钮全部禁用，停止/卸载/只读刷新保留。
    assert.equal(instance.api.canLoad.value, false, '故障锁下不得加载新包');
    assert.equal(instance.api.canPlay.value, false, '故障锁下不得播放');
    assert.equal(instance.api.canPause.value, false);
    assert.equal(instance.api.canResume.value, false);
    assert.equal(instance.api.canBuiltin.value, false, '故障锁下不得恢复测试猫');
    assert.equal(instance.api.canStop.value, true, '故障锁必须保留显式停止');
    assert.equal(instance.api.canUnload.value, true, '故障锁必须保留卸载');

    // 直接组件处理函数与拖放同样不得派发非恢复命令。
    instance.api.selectedAction.value = 'wave';
    await instance.api.playSelected();
    await instance.api.pausePlayback();
    await instance.api.resumePlayback();
    await instance.api.restoreBuiltin();
    await instance.api.handleDropPaths(['C:\\packs\\next']);
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\dragged'] });
    await flush();
    assert.equal(instance.commandCalls.length, 0, '故障锁下不得派发任何非恢复命令');
    assert.match(instance.api.notice.value, /命中区域/);

    // 成功只读刷新若仍是该故障投影，即使 revision 更高也不得解锁。
    const refreshing = instance.api.refreshState();
    await flush();
    assert.equal(instance.stateRequests.length, 2);
    instance.stateRequests[1].resolve(hitMaskFaultState({ revision: 11 }));
    await refreshing;
    assert.equal(instance.api.material.value.revision, 11);
    assert.equal(instance.api.canLoad.value, false);
    assert.equal(instance.api.canPlay.value, false);
    assert.equal(instance.api.canStop.value, true);
    assert.equal(instance.commandCalls.length, 0, '只读刷新不得自动重发命令');

    // 低于当前修订的恢复投影被 revision 排序拒绝，不能解锁。
    instance.emitMaterial(materialState({ revision: 5, status: 'stopped', actionId: null }));
    await flush();
    assert.equal(instance.api.material.value.revision, 11);
    assert.equal(instance.api.canLoad.value, false);

    // 显式恢复失败仍保持锁定。
    const failing = instance.api.stopPlayback();
    await flush();
    assert.equal(instance.commandCalls.length, 1);
    assert.equal(instance.commandCalls[0].command.type, 'stop');
    instance.commandCalls[0].resolve({
      ok: false,
      state: hitMaskFaultState({ revision: 12 }),
      error: { code: 'material_stop_failed', message: '素材停止失败，当前状态需要重新读取', disposition: 'settled' },
    });
    await failing;
    assert.equal(instance.api.material.value.revision, 12);
    assert.equal(instance.api.canLoad.value, false);
    assert.equal(instance.api.canPlay.value, false);
    assert.equal(instance.api.canStop.value, true);

    // 只有更高 revision 的实际恢复状态才恢复正常入口。
    const recovering = instance.api.stopPlayback();
    await flush();
    assert.equal(instance.commandCalls.length, 2);
    instance.commandCalls[1].resolve(
      reply(materialState({ revision: 13, status: 'stopped', actionId: null })),
    );
    await recovering;
    assert.equal(instance.api.material.value.revision, 13);
    assert.equal(instance.api.material.value.error, null);
    assert.equal(instance.api.canLoad.value, true, '实际恢复后加载入口恢复');
    assert.equal(instance.api.canPlay.value, true, '实际恢复后已装动作可播放');
    assert.equal(instance.api.canBuiltin.value, true);

    const replay = instance.api.playSelected();
    await flush();
    assert.equal(instance.commandCalls.length, 3);
    assert.equal(instance.commandCalls[2].command.type, 'play');
    instance.commandCalls[2].resolve(reply(materialState({ revision: 14, status: 'playing', actionId: 'wave' })));
    await replay;
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：empty+故障仍可停止/卸载显式恢复，失败保持锁定，恢复后回到正常入口', async () => {
  const instance = await mountedWithState(
    emptyState({ revision: 3, status: 'faulted', error: { code: HIT_MASK_FAULT_CODE, message: '无法确认旧命中区域已清除' } }),
  );
  try {
    assert.equal(instance.api.canLoad.value, false);
    assert.equal(instance.api.canPlay.value, false, 'empty 本身也不可播放');
    assert.equal(instance.api.canBuiltin.value, false);
    assert.equal(instance.api.canStop.value, true);
    assert.equal(instance.api.canUnload.value, true, 'empty+故障仍须提供卸载恢复入口');

    // 拖放/加载在 empty 故障锁下不得派发。
    instance.emitDrag({ type: 'drop', paths: ['C:\\packs\\next'] });
    await flush();
    assert.equal(instance.commandCalls.length, 0);

    // 显式卸载失败仍锁定。
    const failing = instance.api.unloadMaterial();
    await flush();
    assert.equal(instance.commandCalls.length, 1);
    assert.equal(instance.commandCalls[0].command.type, 'unload');
    instance.commandCalls[0].resolve({
      ok: false,
      state: emptyState({
        revision: 4,
        status: 'faulted',
        error: { code: HIT_MASK_FAULT_CODE, message: '无法确认旧命中区域已清除' },
      }),
      error: { code: 'material_unload_failed', message: '素材卸载失败，当前状态需要重新读取', disposition: 'settled' },
    });
    await failing;
    assert.equal(instance.api.material.value.revision, 4);
    assert.equal(instance.api.canLoad.value, false);
    assert.equal(instance.api.canStop.value, true);
    assert.equal(instance.api.canUnload.value, true);

    // 更高 revision 的实际恢复（empty+stopped, error=null）解除锁。
    const stopping = instance.api.stopPlayback();
    await flush();
    assert.equal(instance.commandCalls.length, 2);
    instance.commandCalls[1].resolve(reply(emptyState({ revision: 5 })));
    await stopping;
    assert.equal(instance.api.canLoad.value, true);
    assert.equal(instance.api.canStop.value, false);
    assert.equal(instance.api.canUnload.value, false);
    assert.equal(instance.api.canPlay.value, false);
  } finally {
    instance.unmount();
  }
});

test('真实素材组件：只按宿主固定错误码识别恢复锁，不按故障文案或 faulted 状态猜测', async () => {
  // 相同文案但不是命中区域清除码：不启用该锁，保持既有故障状态入口。
  const sameMessage = await mountedWithState(
    materialState({
      revision: 2,
      status: 'faulted',
      error: { code: 'material_start_failed', message: '无法确认旧命中区域已清除，请显式重试清除' },
    }),
  );
  try {
    assert.equal(sameMessage.api.canLoad.value, true);
    assert.equal(sameMessage.api.canPlay.value, true);
  } finally {
    sameMessage.unmount();
  }

  // 有固定码但文案不同：仍锁定，说明判定只看码。
  const sameCode = await mountedWithState(
    hitMaskFaultState({ revision: 2, error: { code: HIT_MASK_FAULT_CODE, message: '宿主安全文案不参与判定' } }),
  );
  try {
    assert.equal(sameCode.api.canLoad.value, false);
    assert.equal(sameCode.api.canPlay.value, false);
    assert.equal(sameCode.api.canStop.value, true);
    assert.equal(sameCode.api.canUnload.value, true);
  } finally {
    sameCode.unmount();
  }
});
