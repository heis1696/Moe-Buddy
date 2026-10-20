// Verify an extracted package without --data-dir or a profile override.
// Only fresh target/ directories are allowed; all text and identities are synthetic.
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import path from 'node:path';
import { spawn } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import { CdpClient, freePort, waitForExit } from './native-smoke.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../..');
const executable = path.resolve(process.argv[2] ?? '');
const directory = path.dirname(executable);
const target = path.join(root, 'target') + path.sep;
assert.ok(executable.toLowerCase().startsWith(target.toLowerCase()), 'use a fresh package extracted under target/');
assert.equal(await fs.readFile(path.join(directory, 'moe-buddy.portable'), 'utf8'), '1\n');
const data = path.join(directory, 'data');
await assert.rejects(fs.access(data), { code: 'ENOENT' });
const defaultRoot = path.join(directory, 'unexpected-default');
const report = { executable, data, runs: [], passed: false };
const exitButton = `document.querySelector('[data-testid="quit-application"]')`;
const snapshot = async cdp => (await cdp.invoke('prototype_snapshot')).snapshot;
let firstBuddy;
try {
  for (const run of [1, 2]) {
    const port = await freePort();
    const env = { ...process.env, LOCALAPPDATA: defaultRoot,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-address=127.0.0.1 --remote-debugging-port=${port}` };
    delete env.WEBVIEW2_USER_DATA_FOLDER;
    const child = spawn(executable, [], { cwd: root, env, windowsHide: true, stdio: 'ignore' });
    const entry = { run, pid: child.pid, exit: null };
    report.runs.push(entry);
    let cdp;
    try {
      cdp = await CdpClient.connect(port, '离线验证');
      await cdp.waitFor(`${exitButton}?.disabled === false`, 15000, 'portable app ready');
      const before = await snapshot(cdp);
      assert.ok(before.activeBuddyId);
      if (run === 1) firstBuddy = before.activeBuddyId;
      else assert.equal(before.activeBuddyId, firstBuddy, 'portable restart lost stable identity');
      assert.equal(before.externalTransmissionAllowed, false);
      await fs.access(path.join(data, 'webview'));
      await fs.access(path.join(data, 'buddies.v1'));
      await assert.rejects(fs.access(path.join(defaultRoot, 'MoeBuddy', 'daily')), { code: 'ENOENT' });
      if (run === 1) {
        const draft = 'synthetic portable exit draft';
        await cdp.typeInto('#chat-input', draft);
        await cdp.clickElement(exitButton);
        await cdp.waitFor(`document.querySelector('#close-dialog-title') !== null`, 10000, 'quit guards unsaved draft');
        assert.match(await cdp.evaluate(`document.querySelector('#close-dialog-title').textContent`), /退出/);
        await cdp.clickElement(`document.querySelector('[role="dialog"] button:not(.danger)')`);
        await cdp.waitFor(`document.querySelector('[role="dialog"]') === null`, 5000, 'quit cancelled');
        assert.equal(child.exitCode, null);
        assert.equal((await snapshot(cdp)).chat.draft, draft);
        await cdp.clickElement(exitButton);
        await cdp.clickElement(`document.querySelector('[role="dialog"] button.danger')`);
        entry.draftCancelThenConfirm = true;
      } else {
        assert.equal(before.chat.draft, '');
        await cdp.clickElement(exitButton);
        entry.restoredSameBuddy = true;
      }
      entry.exit = await waitForExit(child);
      assert.equal(entry.exit.code, 0);
      assert.equal(entry.exit.signal, null);
    } finally {
      // On failure use the normal exit path, confirming only this test's synthetic drafts.
      if (child.exitCode === null && cdp) {
        try {
          const state = await snapshot(cdp);
          await cdp.invoke('prototype_window', { command: 'quit', context: {
            expectedBuddyId: state.activeBuddyId ?? '', expectedBuddiesRevision: state.buddiesRevision,
          } });
          for (let index = 0; index < 2 && child.exitCode === null; index++) {
            await cdp.clickElement(`document.querySelector('[role="dialog"] button.danger')`).catch(() => {});
          }
          entry.cleanupExit = await waitForExit(child);
        } catch (error) { entry.cleanupError = error.message; }
      }
      cdp?.close();
    }
  }
  report.passed = true;
} finally {
  await fs.writeFile(path.join(directory, '..', 'portable-smoke.json'), JSON.stringify(report, null, 2));
  console.log(JSON.stringify(report, null, 2));
}
