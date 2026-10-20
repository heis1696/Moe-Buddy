const test = require('node:test');
const assert = require('node:assert/strict');

const { completeModelSettingsApply } = require('../.test-build/modelConfigApply.js');
const { modelFormFromSettings } = require('../.test-build/modelConfig.js');
const { RevisionOrderedSnapshots } = require('../.test-build/snapshot.js');

function settings(persona) {
  return {
    endpoint: 'https://example.invalid/v1/chat/completions',
    model: 'synthetic',
    persona,
    allowTransmission: false,
    hasApiKey: false,
  };
}

function snapshot(revision, modelSettings, overrides = {}) {
  return {
    revision,
    presentationRevision: revision,
    mode: 'offline',
    revoked: false,
    chat: { draft: '', messages: [], status: 'ready', failure: null },
    closeConfirmation: false,
    closeIntent: null,
    managementDraft: { version: 0, dirty: false },
    managementExit: null,
    managementDiscardedVersion: null,
    chatDraft: { revision, discard: null, sessions: [] },
    activePreset: '',
    externalTransmissionAllowed: false,
    presets: [],
    memories: [],
    diagnostics: [],
    notice: null,
    modelSettings,
    persistenceEnabled: true,
    ...overrides,
  };
}

function deferred() {
  let resolve;
  const promise = new Promise((complete) => {
    resolve = complete;
  });
  return { promise, resolve };
}

function flow(store, pending, state, applied) {
  return {
    dispatch: () => pending.promise,
    applyResponse: (response) => store.applyResponse(response),
    currentSnapshot: () => store.current,
    componentIsActive: () => state.componentActive,
    interactionIsCurrent: () => state.interactionCurrent,
    onApplied: applied,
  };
}

test('event-first result still completes an effective invoke when its response is already applied', async () => {
  const store = new RevisionOrderedSnapshots();
  const responseSnapshot = snapshot(4, settings('synthetic current persona'));
  store.applyResponse({ snapshot: responseSnapshot, error: null });
  const pending = deferred();
  const state = { componentActive: true, interactionCurrent: true };
  const completed = [];

  const result = completeModelSettingsApply(flow(store, pending, state, (form) => completed.push(form)));
  pending.resolve({ snapshot: responseSnapshot, error: null });
  const update = await result;

  assert.equal(update.kind, 'success');
  assert.equal(update.applied, false);
  assert.deepEqual(completed, [modelFormFromSettings(responseSnapshot.modelSettings)]);
});

test('successful invoke refills from the current effective snapshot, not its older response', async () => {
  const store = new RevisionOrderedSnapshots();
  const current = snapshot(8, settings('newer event persona'));
  store.applyResponse({ snapshot: current, error: null });
  const pending = deferred();
  const state = { componentActive: true, interactionCurrent: true };
  const completed = [];

  const result = completeModelSettingsApply(flow(store, pending, state, (form) => completed.push(form)));
  pending.resolve({ snapshot: snapshot(7, settings('older invoke persona')), error: null });
  await result;

  assert.deepEqual(completed, [modelFormFromSettings(current.modelSettings)]);
});

test('revocation arriving during invoke prevents old configuration from refilling the form', async () => {
  const store = new RevisionOrderedSnapshots();
  store.applyResponse({
    snapshot: snapshot(8, null, { revoked: true }),
    error: null,
  });
  const pending = deferred();
  const state = { componentActive: true, interactionCurrent: true };
  const completed = [];

  const result = completeModelSettingsApply(flow(store, pending, state, (form) => completed.push(form)));
  pending.resolve({ snapshot: snapshot(7, settings('revoked persona')), error: null });
  await result;

  assert.deepEqual(completed, []);
  assert.equal(store.current.revoked, true);
});

test('released components and superseded interactions do not receive async form callbacks', async () => {
  const response = { snapshot: snapshot(4, settings('late persona')), error: null };
  const releasedStore = new RevisionOrderedSnapshots();
  const releasedPending = deferred();
  const releasedState = { componentActive: true, interactionCurrent: true };
  const releasedCalls = [];
  const releasedResult = completeModelSettingsApply(
    flow(releasedStore, releasedPending, releasedState, (form) => releasedCalls.push(form)),
  );
  releasedState.componentActive = false;
  releasedPending.resolve(response);
  assert.equal(await releasedResult, null);
  assert.equal(releasedStore.current, null);
  assert.deepEqual(releasedCalls, []);

  const currentStore = new RevisionOrderedSnapshots();
  const oldPending = deferred();
  const oldState = { componentActive: true, interactionCurrent: true };
  const oldCalls = [];
  const oldResult = completeModelSettingsApply(
    flow(currentStore, oldPending, oldState, (form) => oldCalls.push(form)),
  );
  oldState.interactionCurrent = false;
  oldPending.resolve(response);
  await oldResult;
  assert.equal(currentStore.current.modelSettings.persona, 'late persona');
  assert.deepEqual(oldCalls, []);
});
