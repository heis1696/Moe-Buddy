const test = require('node:test');
const assert = require('node:assert/strict');

const {
  consumeManagementDiscardReceipt,
  ManagementDraftSync,
} = require('../.test-build/managementDraftSync.js');
const { RevisionOrderedSnapshots } = require('../.test-build/snapshot.js');

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

const SCOPE_A = { expectedBuddyId: 'buddy-a', expectedBuddiesRevision: 6 };
const SCOPE_B = { expectedBuddyId: 'buddy-b', expectedBuddiesRevision: 6 };

test('management dirty updates are bounded and every edit gets a newer version', async () => {
  const calls = [];
  const sync = new ManagementDraftSync(SCOPE_A,
    (version, dirty) =>
      new Promise((resolve) => calls.push({ version, dirty, resolve })),
  );

  sync.record(true);
  for (let index = 0; index < 500; index += 1) sync.record(true);
  assert.equal(calls.length, 1);
  assert.equal(calls[0].version, 1);
  assert.equal(sync.version, 501);
  assert.equal(sync.dirty, true);

  const flush = sync.flush();
  calls[0].resolve({ version: 1, dirty: true });
  await tick();
  assert.equal(calls.length, 2);
  assert.equal(calls[1].version, 501);
  calls[1].resolve({ version: 501, dirty: true });

  assert.equal(await flush, true);
  assert.equal(sync.isSynced, true);
});

test('failed status delivery stays unconfirmed until an explicit flush succeeds', async () => {
  const calls = [];
  const sync = new ManagementDraftSync(SCOPE_A, (version, dirty) => {
    calls.push({ version, dirty });
    if (calls.length === 1) return Promise.reject(new Error('synthetic IPC failure'));
    return Promise.resolve({ version, dirty });
  });

  sync.record(true);
  await tick();
  assert.equal(sync.isSynced, false);
  assert.equal(await sync.flush(), true);
  assert.deepEqual(calls, [
    { version: 1, dirty: true },
    { version: 1, dirty: true },
  ]);
});

test('adopt preserves a host version and stale acknowledgements do not confirm it', async () => {
  const sync = new ManagementDraftSync(SCOPE_A, async (version, dirty) => ({
    version: version - 1,
    dirty,
  }));
  sync.adopt({ version: 9, dirty: false, kind: 'none' }, SCOPE_A);
  assert.equal(sync.version, 9);
  assert.equal(sync.isSynced, true);

  sync.record(true);
  assert.equal(await sync.flush(), false);
  assert.equal(sync.version, 10);
  assert.equal(sync.dirty, true);
});

test('matching discard receipt is consumed once and an older receipt cannot clear a newer edit', async () => {
  const sync = new ManagementDraftSync(SCOPE_A, async (version, dirty) => ({ version, dirty }));
  sync.record(true);
  assert.equal(await sync.flush(), true);

  assert.equal(sync.consumeDiscardedVersion(1), true);
  assert.equal(sync.dirty, false);
  assert.equal(sync.isSynced, true);
  assert.equal(sync.consumeDiscardedVersion(1), false);

  sync.record(true);
  assert.equal(sync.version, 2);
  assert.equal(sync.consumeDiscardedVersion(1), false);
  assert.equal(sync.dirty, true);
  assert.equal(sync.consumeDiscardedVersion(2), false, 'an unconfirmed edit is not cleared');
  assert.equal(await sync.flush(), true);
  assert.equal(sync.consumeDiscardedVersion(2), true);
  sync.adopt({ version: 2, dirty: true, kind: 'general' }, SCOPE_A);
  assert.equal(sync.dirty, false, 'a repeated stale snapshot cannot restore the discarded edit');
});

test('discard receipt consumption is shared by events, responses, rereads, and reopened snapshots', async () => {
  const sync = new ManagementDraftSync(SCOPE_A, async (version, dirty) => ({ version, dirty }));
  sync.record(true);
  await sync.flush();
  let cleared = 0;
  const store = new RevisionOrderedSnapshots();
  const makeSnapshot = (presentationRevision, dirty, discardedVersion) => ({
    revision: presentationRevision,
    presentationRevision,
    mode: 'offline',
    revoked: false,
    chat: { draft: '', messages: [], status: 'ready', failure: null },
    closeConfirmation: false,
    closeIntent: null,
    managementDraft: { version: 1, dirty, kind: dirty ? 'general' : 'none' },
    managementExit: null,
    managementDiscardedVersion: discardedVersion,
    chatDraft: { revision: presentationRevision, discard: null, sessions: [] },
    activePreset: 'offline-echo',
    externalTransmissionAllowed: false,
    presets: [],
    memories: [],
    diagnostics: [],
    notice: null,
    modelSettings: null,
    persistenceEnabled: true,
  });
  const receipt = makeSnapshot(3, false, 1);

  store.applyResponse({ snapshot: receipt, error: null });
  assert.equal(consumeManagementDiscardReceipt(receipt, sync, () => { cleared += 1; }), true);
  assert.equal(sync.dirty, false, 'an event arriving before the invoke response clears local dirty state');

  store.applyResponse({ snapshot: receipt, error: null });
  assert.equal(consumeManagementDiscardReceipt(receipt, sync, () => { cleared += 1; }), false);
  assert.equal(cleared, 1, 'the later response or reread is idempotent');

  sync.record(true);
  await sync.flush();
  const oldReceipt = makeSnapshot(2, false, 1);
  consumeManagementDiscardReceipt(oldReceipt, sync, () => { cleared += 1; });
  assert.equal(sync.version, 2);
  assert.equal(sync.dirty, true, 'an older receipt does not clear a newer edit');

  store.reset();
  store.applyResponse({ snapshot: receipt, error: null });
  consumeManagementDiscardReceipt(receipt, sync, () => { cleared += 1; });
  assert.equal(sync.dirty, true, 'a hidden reopen or reread cannot restore the prior clean version');
  assert.equal(cleared, 1);
});

test('invoke response may consume the receipt before the matching snapshot event', async () => {
  const sync = new ManagementDraftSync(SCOPE_A, async (version, dirty) => ({ version, dirty }));
  sync.record(true);
  await sync.flush();
  let cleared = 0;
  const receipt = { managementDiscardedVersion: 1 };

  assert.equal(consumeManagementDiscardReceipt(receipt, sync, () => { cleared += 1; }), true);
  assert.equal(consumeManagementDiscardReceipt(receipt, sync, () => { cleared += 1; }), false);
  assert.equal(sync.dirty, false);
  assert.equal(cleared, 1);
});

test('a matching snapshot event alone consumes a discard when the invoke response is lost', async () => {
  const sync = new ManagementDraftSync(SCOPE_A, async (version, dirty) => ({ version, dirty }));
  sync.record(true);
  await sync.flush();
  let cleared = 0;

  assert.equal(
    consumeManagementDiscardReceipt({ managementDiscardedVersion: 1 }, sync, () => { cleared += 1; }),
    true,
  );
  assert.equal(sync.dirty, false);
  assert.equal(sync.isSynced, true);
  assert.equal(cleared, 1);
});

test('kind is a required dispatch payload and clean state is always none', async () => {
  const calls = [];
  const sync = new ManagementDraftSync(SCOPE_A, async (version, dirty, kind) => {
    calls.push({ version, dirty, kind });
    return { version, dirty };
  });
  assert.equal(sync.record(true, 'buddy_create'), true);
  assert.equal(sync.kind, 'buddy_create');
  assert.equal(await sync.flush(), true);
  assert.deepEqual(calls, [{ version: 1, dirty: true, kind: 'buddy_create' }]);

  assert.equal(sync.record(false, 'general'), false, 'clean state rejects a non-none kind');
  assert.equal(sync.record(true, 'none'), false, 'dirty state rejects none');
  assert.equal(sync.version, 1);

  sync.record(false, 'none');
  assert.equal(await sync.flush(), true);
  assert.deepEqual(calls[calls.length - 1], { version: 2, dirty: false, kind: 'none' });
  assert.equal(sync.kind, 'none');
});

test('adopt and discard receipts only accept a valid dirty/kind/version tuple', async () => {
  const sync = new ManagementDraftSync(SCOPE_A, async (version, dirty) => ({ version, dirty }));
  assert.equal(sync.adopt({ version: 4, dirty: true, kind: 'none' }, SCOPE_A), false);
  assert.equal(sync.adopt({ version: 4, dirty: false, kind: 'general' }, SCOPE_A), false);
  assert.equal(sync.adopt({ version: 4, dirty: false, kind: 'none' }, SCOPE_A), true);
  assert.equal(sync.kind, 'none');

  assert.equal(sync.record(true, 'buddy_rename'), true);
  assert.equal(await sync.flush(), true);
  assert.equal(sync.consumeDiscardedVersion(5), true);
  assert.equal(sync.kind, 'none');
  assert.equal(sync.dirty, false);
});
