const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const ts = require('typescript');
const vue = require('vue');

// Execute the actual component script with controlled IPC completion. This does
// not add a production inspection API or pretend to verify DOM rendering.
function component() {
  const sourcePath = path.join(__dirname, '../src/app/MainView.vue');
  const source = fs.readFileSync(sourcePath, 'utf8').match(/<script setup lang="ts">([\s\S]*?)<\/script>/)[1];
  const script = ts.transpileModule(source + `
    module.exports.probe = {
      applyResponse, beginEditMemory, saveMemory,
      activePanel, memoryDraft, memoryEdit, snapshot, snapshots,
    };
  `, { compilerOptions: { module: ts.ModuleKind.CommonJS, target: ts.ScriptTarget.ES2022 } }).outputText;
  let complete;
  const pending = new Promise((resolve) => { complete = resolve; });
  const beforeUnmount = [];
  const scope = vue.effectScope();
  const exported = { exports: {} };
  const context = {
    module: exported,
    exports: exported.exports,
    setTimeout,
    clearTimeout,
    require(name) {
      if (name === 'vue') return {
        ...vue,
        onMounted() {},
        onBeforeUnmount(callback) { beforeUnmount.push(callback); },
      };
      if (name === '../platform/tauri') return {
        dispatchPrototype: () => pending,
        updateManagementDraftState: async (version, dirty, kind) => ({ version, dirty, kind }),
      };
      if (name === './MaterialPanel.vue') return {};
      if (name.startsWith('../core/')) {
        return require(path.join(__dirname, '../.test-build', name.slice('../core/'.length) + '.js'));
      }
      throw new Error(`Unexpected component dependency: ${name}`);
    },
  };
  scope.run(() => vm.runInNewContext(script, context, { filename: sourcePath }));
  let unmounted = false;
  return {
    api: exported.exports.probe,
    complete,
    unmount() {
      if (unmounted) return;
      unmounted = true;
      for (const callback of beforeUnmount) callback();
      scope.stop();
    },
  };
}

function snapshot(revision) {
  return {
    revision, presentationRevision: revision, mode: 'offline', revoked: false,
    chat: { draft: '', messages: [], status: 'ready', failure: null },
    closeConfirmation: false, closeIntent: null,
    managementDraft: { version: 0, dirty: false, kind: 'none' }, managementExit: null,
    managementDiscardedVersion: null, chatDraft: { revision: 1, discard: null, sessions: [] },
    activePreset: '', externalTransmissionAllowed: false,
    presets: [], memories: [{ id: '1', revision: 1, text: 'synthetic private note' }],
    diagnostics: [], notice: null, modelSettings: null, persistenceEnabled: true,
    activeBuddyId: 'buddy-a', buddiesRevision: 2,
    activeBuddyPersona: '伙伴 A 的合成人设',
    buddyCommit: null,
    buddies: [{ id: 'buddy-a', label: 'A', profileRevision: 1, personaRevision: 1, modelRevision: 1 }],
  };
}

function conflictResponse() {
  return {
    snapshot: snapshot(2),
    error: {
      code: 'memory_revision_conflict', message: 'Synthetic conflict', disposition: 'not_executed',
    },
  };
}

function beginSave(instance) {
  const api = instance.api;
  const initial = snapshot(1);
  api.applyResponse({ snapshot: initial, error: null }, 'load');
  api.activePanel.value = 'memory';
  api.beginEditMemory(initial.memories[0]);
  api.memoryDraft.value = 'synthetic retained edit';
  return api.saveMemory();
}

test('a late real memory callback cannot repopulate a released component or its snapshot cache', async () => {
  const instance = component();
  try {
    const saving = beginSave(instance);
    instance.unmount();
    instance.complete(conflictResponse());
    await saving;
    assert.equal(instance.api.memoryEdit.value, null);
    assert.equal(instance.api.memoryDraft.value, '');
    assert.equal(instance.api.snapshot.value, null);
    assert.equal(instance.api.snapshots.current, null);
  } finally {
    instance.unmount();
  }
});

test('the same active component callback retains a failed memory edit', async () => {
  const instance = component();
  try {
    const saving = beginSave(instance);
    instance.complete(conflictResponse());
    await saving;
    assert.equal(instance.api.memoryEdit.value.id, '1');
    assert.equal(instance.api.memoryEdit.value.baseText, 'synthetic private note');
    assert.equal(instance.api.memoryDraft.value, 'synthetic retained edit');
    assert.equal(instance.api.snapshots.current.revision, 2);
  } finally {
    instance.unmount();
  }
});
