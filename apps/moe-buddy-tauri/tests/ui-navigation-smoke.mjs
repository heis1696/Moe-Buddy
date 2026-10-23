import assert from 'node:assert/strict';
import { execFile, spawn } from 'node:child_process';
import fs from 'node:fs/promises';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const workspaceRoot = path.resolve(appRoot, '..', '..');
const outputRoot = path.join(workspaceRoot, 'target', 'ui-navigation-smoke');
const binary = process.env.MOE_SMOKE_BINARY
  ? path.resolve(process.env.MOE_SMOKE_BINARY)
  : path.join(workspaceRoot, 'target', 'debug', process.platform === 'win32' ? 'moe-buddy-tauri.exe' : 'moe-buddy-tauri');
const nativeWindowScript = path.join(appRoot, 'tests', 'native-window.ps1');
const pages = ['chat', 'buddies', 'appearance', 'memory', 'model', 'diagnostics'];
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const visible = (node) => !!node && !!(node.offsetWidth || node.offsetHeight || node.getClientRects().length);

async function freePort() {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const port = server.address().port;
      server.close(() => resolve(port));
    });
  });
}

function resolveElement(expression) {
  if (expression.startsWith('#') || expression.startsWith('.') || expression.startsWith('[')) {
    return `document.querySelector(${JSON.stringify(expression)})`;
  }
  return expression;
}

class CdpClient {
  static async connect(port) {
    const deadline = Date.now() + 30_000;
    const failures = [];
    while (Date.now() < deadline) {
      try {
        const targets = await fetch(`http://127.0.0.1:${port}/json/list`, { signal: AbortSignal.timeout(2_000) }).then((r) => r.json());
        for (const target of targets.filter((entry) => entry.type === 'page' && entry.title.includes('Moe Buddy'))) {
          let socket;
          try {
            socket = new WebSocket(target.webSocketDebuggerUrl);
            await new Promise((resolve, reject) => {
              const timer = setTimeout(() => reject(new Error('CDP websocket connection timed out')), 1_500);
              socket.onopen = () => { clearTimeout(timer); resolve(); };
              socket.onerror = () => { clearTimeout(timer); reject(new Error('CDP websocket failed to connect')); };
              socket.onclose = () => { clearTimeout(timer); reject(new Error('CDP websocket closed while connecting')); };
            });
            const cdp = new CdpClient(socket);
            const isMain = await cdp.evaluate(`document.documentElement.dataset.window === 'main' && document.querySelector('.main-view') !== null`);
            if (isMain === true) return cdp;
            cdp.close();
          } catch (error) {
            socket?.close();
            failures.push(`${target.title} ${target.url}: ${error.message}`);
          }
        }
      } catch {}
      await sleep(250);
    }
    throw new Error(`main WebView2 CDP target not found; candidates=${failures.slice(-8).join(' | ')}`);
  }

  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    this.closed = false;
    socket.onmessage = (event) => {
      let message;
      try { message = JSON.parse(event.data); } catch (error) { this.failAll(new Error(`invalid CDP response: ${error.message}`)); return; }
      const waiter = this.pending.get(message.id);
      if (!waiter) return;
      clearTimeout(waiter.timer);
      this.pending.delete(message.id);
      if (message.error) waiter.reject(new Error(message.error.message ?? 'CDP command failed'));
      else waiter.resolve(message);
    };
    socket.onerror = () => this.failAll(new Error('CDP websocket error'));
    socket.onclose = () => { this.closed = true; this.failAll(new Error('CDP websocket closed')); };
  }

  failAll(error) {
    for (const waiter of this.pending.values()) {
      clearTimeout(waiter.timer);
      waiter.reject(error);
    }
    this.pending.clear();
  }

  send(method, params = {}) {
    if (this.closed || this.socket.readyState !== WebSocket.OPEN) return Promise.reject(new Error('CDP websocket is not open'));
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`CDP request timed out: ${method}`));
      }, 10_000);
      this.pending.set(id, { resolve, reject, timer });
      try { this.socket.send(JSON.stringify({ id, method, params })); }
      catch (error) { clearTimeout(timer); this.pending.delete(id); reject(error); }
    });
  }

  async evaluate(expression) {
    const response = await this.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (response.result?.exceptionDetails) throw new Error(`CDP evaluate failed: ${JSON.stringify(response.result.exceptionDetails)}`);
    return response.result?.result?.value;
  }

  async waitFor(expression, timeout, label) {
    const deadline = Date.now() + timeout;
    let last;
    while (Date.now() < deadline) {
      try {
        last = await this.evaluate(`Boolean(${expression})`);
        if (last === true) return;
      } catch (error) { last = error.message; }
      await sleep(100);
    }
    throw new Error(`timed out waiting for ${label}; last=${String(last)}`);
  }

  async clickElement(expression) {
    const target = resolveElement(expression);
    await this.waitFor(`(()=>{const e=${target};return !!e&&!e.disabled&&(${visible.toString()})(e)})()`, 5_000, 'visible enabled click target');
    const point = await this.evaluate(`(()=>{const e=${target};e.scrollIntoView({block:'center',inline:'center'});const r=e.getBoundingClientRect();return {x:r.left+r.width/2,y:r.top+r.height/2}})()`);
    assert.ok(point && point.x >= 0 && point.y >= 0, `invalid click point for ${expression}`);
    await this.send('Input.dispatchMouseEvent', { type: 'mouseMoved', x: point.x, y: point.y });
    await this.send('Input.dispatchMouseEvent', { type: 'mousePressed', x: point.x, y: point.y, button: 'left', clickCount: 1 });
    await this.send('Input.dispatchMouseEvent', { type: 'mouseReleased', x: point.x, y: point.y, button: 'left', clickCount: 1 });
  }

  async typeInto(expression, text) {
    const target = resolveElement(expression);
    await this.waitFor(`(()=>{const e=${target};return !!e&&!e.disabled&&(${visible.toString()})(e)})()`, 5_000, 'visible enabled input');
    await this.evaluate(`(()=>{const e=${target};e.focus();e.value='';e.dispatchEvent(new Event('input',{bubbles:true}));return true})()`);
    if (text) await this.send('Input.insertText', { text });
    await this.waitFor(`(()=>{const e=${target};return !!e&&e.value===${JSON.stringify(text)}})()`, 5_000, 'typed input value');
  }

  close() {
    if (!this.closed) this.socket.close();
  }
}

function native(args) {
  return new Promise((resolve, reject) => execFile('powershell.exe', ['-NoProfile', '-File', nativeWindowScript, ...args], { windowsHide: true, timeout: 30_000 }, (error, stdout, stderr) => {
    if (error) return reject(new Error(`native helper failed: ${stdout.trim()} ${stderr.trim()} (${error.message})`));
    try { resolve(JSON.parse(stdout.trim())); } catch { reject(new Error(`invalid native helper output: ${stdout.trim()} ${stderr.trim()}`)); }
  }));
}

function waitForExit(child, timeout = 20_000) {
  if (child.exitCode !== null) return Promise.resolve({ code: child.exitCode, signal: child.signalCode });
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error(`host did not exit within ${timeout}ms`)), timeout);
    child.once('exit', (code, signal) => { clearTimeout(timer); resolve({ code, signal }); });
  });
}

async function main() {
  const stamp = new Date().toISOString().replace(/[:.]/g, '-');
  const runRoot = path.join(outputRoot, stamp);
  const dataDir = path.join(runRoot, 'data');
  const profile = path.join(runRoot, 'profile');
  await fs.mkdir(dataDir, { recursive: true });
  await fs.mkdir(profile, { recursive: true });
  const port = await freePort();
  const result = { startedAt: new Date().toISOString(), binary, dataDir, profile, cdpPort: port, pages: {}, checks: [], screenshots: [], status: 'failed', cleanup: {} };
  const logs = [];
  let spawnError;
  const child = spawn(binary, ['--data-dir', dataDir], {
    env: { ...process.env, LOCALAPPDATA: dataDir, WEBVIEW2_USER_DATA_FOLDER: profile, WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port} --remote-allow-origins=*` },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.once('error', (error) => { spawnError = error; logs.push(`spawn: ${error.message}`); });
  child.stdout.on('data', (chunk) => logs.push(`stdout: ${chunk.toString()}`));
  child.stderr.on('data', (chunk) => logs.push(`stderr: ${chunk.toString()}`));
  let cdp;
  try {
    await new Promise((resolve, reject) => {
      if (child.pid) return resolve();
      child.once('spawn', resolve);
      child.once('error', reject);
      setTimeout(() => reject(spawnError ?? new Error('host spawn timed out')), 10_000);
    });
    result.pid = child.pid;
    cdp = await CdpClient.connect(port);
    await cdp.send('Page.enable');
    await cdp.waitFor('window.__TAURI_INTERNALS__ !== undefined', 15_000, 'Tauri bridge');
    await cdp.waitFor(`document.querySelector('.main-view') !== null`, 20_000, 'Vue main view');
    await cdp.waitFor(`document.querySelectorAll('.sidebar .nav-item[data-page]').length===6`, 30_000, 'six production sidebar navigation entries');
    await cdp.waitFor(`document.querySelectorAll('[data-page-view]').length===6`, 30_000, 'connected page content');
    const contract = await cdp.evaluate(`({nav:[...document.querySelectorAll('.sidebar .nav-item[data-page]')].map(n=>n.dataset.page),views:[...document.querySelectorAll('[data-page-view]')].map(n=>n.dataset.pageView)})`);
    assert.deepEqual(contract.nav, pages, 'six sidebar navigation entries differ from production contract');
    assert.deepEqual(contract.views, pages, 'six page views differ from production contract');
    result.mainTarget = await cdp.evaluate(`({title:document.title,url:location.href})`);
    for (const page of pages) {
      await cdp.clickElement(`.sidebar .nav-item[data-page="${page}"]`);
      await cdp.waitFor(`document.querySelector('.sidebar .nav-item[aria-current="page"]')?.dataset.page===${JSON.stringify(page)}&&(()=>{const n=document.querySelector('[data-page-view="${page}"]');return n&&(${visible.toString()})(n)})()`, 8_000, `${page} page navigation`);
      const evidence = await cdp.evaluate(`(()=>{const nav=[...document.querySelectorAll('.sidebar .nav-item[data-page]')];const views=[...document.querySelectorAll('[data-page-view]')];const active=document.querySelector('.sidebar .nav-item[aria-current="page"]')?.dataset.page;const visibleViews=views.filter(n=>(${visible.toString()})(n)).map(n=>n.dataset.pageView);const root=document.documentElement;const input=document.querySelector('#chat-input');return {active,visibleViews,navCount:nav.length,scrollWidth:root.scrollWidth,clientWidth:root.clientWidth,innerWidth,innerHeight,composerVisible:!!input&&(${visible.toString()})(input),pageText:document.querySelector('[data-page-view="${page}"]')?.innerText.slice(0,600)}})()`);
      result.pages[page] = evidence;
      assert.equal(evidence.active, page, `${page} active sidebar navigation mismatch`);
      assert.deepEqual(evidence.visibleViews, [page], `${page} visible view mismatch`);
      assert.equal(evidence.navCount, 6, 'sidebar navigation must contain exactly six entries');
      assert.ok(evidence.scrollWidth <= evidence.clientWidth, `${page} has horizontal overflow`);
      const shot = await cdp.send('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
      const shotPath = path.join(runRoot, `${page}.png`);
      await fs.writeFile(shotPath, Buffer.from(shot.result.data, 'base64'));
      result.screenshots.push(shotPath);
    }
    result.checks.push({ id: 'six-page-navigation', status: 'passed' });

    await cdp.clickElement('.sidebar .nav-item[data-page="chat"]');
    await cdp.waitFor(`document.querySelector('.sidebar .nav-item[aria-current="page"]')?.dataset.page==='chat'`, 5_000, 'chat before layout checks');
    const normal = await cdp.evaluate(`(()=>{const r=document.documentElement;const e=document.querySelector('#chat-input');return {innerWidth,innerHeight,scrollWidth:r.scrollWidth,clientWidth:r.clientWidth,composerVisible:!!e&&(${visible.toString()})(e)}})()`);
    assert.ok(normal.innerWidth >= 900 && normal.innerHeight >= 650, `native main window is smaller than baseline: ${JSON.stringify(normal)}`);
    assert.ok(normal.scrollWidth <= normal.clientWidth && normal.composerVisible, 'baseline chat layout is not usable');
    result.checks.push({ id: 'native-window-layout', status: 'passed', evidence: normal, viewportMode: 'native-window' });

    const resize = await native(['-Action', 'resize-window', '-RootPid', String(child.pid), '-WindowTitle', 'Moe Buddy', '-Width', '640', '-Height', '720']);
    assert.equal(resize.ok, true, `native window resize failed: ${JSON.stringify(resize)}`);
    await sleep(500);
    const narrow = await cdp.evaluate(`(()=>{const r=document.documentElement;const e=document.querySelector('#chat-input');return {innerWidth,innerHeight,scrollWidth:r.scrollWidth,clientWidth:r.clientWidth,composerVisible:!!e&&(${visible.toString()})(e)}})()`);
    result.narrowWindow = { helper: resize, viewport: narrow, viewportMode: 'native-window' };
    assert.ok(narrow.innerWidth <= 680 && narrow.innerWidth >= 500, `native narrow window width out of expected range: ${JSON.stringify(narrow)}`);
    assert.ok(narrow.scrollWidth <= narrow.clientWidth, 'narrow native window has horizontal overflow');
    assert.ok(narrow.composerVisible, 'chat composer is not visible in narrow native window');
    result.checks.push({ id: 'narrow-native-window-layout', status: 'passed', evidence: result.narrowWindow });
    await native(['-Action', 'resize-window', '-RootPid', String(child.pid), '-WindowTitle', 'Moe Buddy', '-Width', '960', '-Height', '720']);
    await sleep(300);

    await cdp.typeInto('#chat-input', 'synthetic cross-page draft');
    await cdp.clickElement('.sidebar .nav-item[data-page="memory"]');
    await cdp.waitFor(`document.querySelector('.sidebar .nav-item[aria-current="page"]')?.dataset.page==='memory'`, 5_000, 'memory after chat draft');
    await cdp.clickElement('.sidebar .nav-item[data-page="chat"]');
    await cdp.waitFor(`document.querySelector('.sidebar .nav-item[aria-current="page"]')?.dataset.page==='chat'`, 5_000, 'chat after returning from memory');
    const draft = await cdp.evaluate(`document.querySelector('#chat-input')?.value ?? null`);
    assert.equal(draft, 'synthetic cross-page draft', 'chat draft did not survive page navigation');
    result.checks.push({ id: 'chat-draft-across-pages', status: 'passed' });

    await cdp.clickElement('.sidebar .nav-item[data-page="memory"]');
    await cdp.waitFor(`document.querySelector('.sidebar .nav-item[aria-current="page"]')?.dataset.page==='memory'`, 5_000, 'memory page');
    await cdp.clickElement('.sidebar .nav-item[data-page="memory"]');
    await cdp.waitFor(`document.querySelector('.remember-form textarea') !== null`, 5_000, 'memory remember form textarea');
    await cdp.typeInto('.remember-form textarea', 'synthetic unsaved memory draft');
    await cdp.clickElement('.sidebar .nav-item[data-page="chat"]');
    const guard = await cdp.evaluate(`(()=>{const d=document.querySelector('[role="dialog"],[role="alertdialog"]');return {text:d?.innerText??'',buttons:[...(d?.querySelectorAll('button')??[])].map(n=>n.textContent.trim())}})()`);
    assert.ok(guard.text, 'dirty memory navigation did not request confirmation');
    assert.ok(guard.buttons.includes('继续编辑'), 'memory navigation confirmation lacks the real cancel action');
    await cdp.clickElement(`Array.from(document.querySelectorAll('[role="dialog"] button,[role="alertdialog"] button')).find(n=>n.textContent.trim()==='继续编辑')`);
    await cdp.waitFor(`document.querySelector('.sidebar .nav-item[aria-current="page"]')?.dataset.page==='memory'`, 5_000, 'memory page retained after continue editing');
    assert.equal(await cdp.evaluate(`document.querySelector('.remember-form textarea')?.value ?? null`), 'synthetic unsaved memory draft', 'canceling navigation lost memory draft');
    result.checks.push({ id: 'memory-dirty-navigation-cancel', status: 'passed', evidence: { dialog: guard.text } });
    await cdp.clickElement('.sidebar .nav-item[data-page="chat"]');
    await cdp.clickElement(`Array.from(document.querySelectorAll('[role="dialog"] button,[role="alertdialog"] button')).find(n=>n.textContent.trim()==='放弃并离开')`);
    await cdp.waitFor(`document.querySelector('.sidebar .nav-item[aria-current="page"]')?.dataset.page==='chat'`, 5_000, 'chat after confirmed memory discard');
    result.checks.push({ id: 'memory-dirty-navigation-confirm', status: 'passed' });

    await cdp.clickElement('.sidebar .nav-item[data-page="model"]');
    const modelField = `Array.from(document.querySelectorAll('[data-page-view="model"] input:not([type="checkbox"]):not([type="password"]),[data-page-view="model"] textarea')).find(n=>!!(n.offsetWidth||n.offsetHeight||n.getClientRects().length))`;
    await cdp.clickElement('.sidebar .nav-item[data-page="model"]');
    await cdp.waitFor(`${modelField} !== undefined`, 5_000, 'model form reopened');
    assert.ok(await cdp.evaluate(`${modelField} !== undefined`), 'model page has no visible editable field');
    await cdp.typeInto(modelField, 'synthetic unsaved model draft');
    await cdp.clickElement('.sidebar .nav-item[data-page="diagnostics"]');
    const modelGuard = await cdp.evaluate(`(()=>{const d=document.querySelector('[role="dialog"],[role="alertdialog"]');return {text:d?.innerText??'',buttons:[...(d?.querySelectorAll('button')??[])].map(n=>n.textContent.trim())}})()`);
    assert.ok(modelGuard.text && modelGuard.buttons.includes('继续编辑') && modelGuard.buttons.includes('放弃并离开'), 'dirty model navigation did not offer cancel and discard');
    await cdp.clickElement(`Array.from(document.querySelectorAll('[role="dialog"] button,[role="alertdialog"] button')).find(n=>n.textContent.trim()==='继续编辑')`);
    await cdp.waitFor(`document.querySelector('.sidebar .nav-item[aria-current="page"]')?.dataset.page==='model'`, 5_000, 'model retained after continue editing');
    assert.equal(await cdp.evaluate(`${modelField}?.value ?? null`), 'synthetic unsaved model draft', 'canceling model navigation lost draft');
    await cdp.clickElement('.sidebar .nav-item[data-page="diagnostics"]');
    await cdp.clickElement(`Array.from(document.querySelectorAll('[role="dialog"] button,[role="alertdialog"] button')).find(n=>n.textContent.trim()==='放弃并离开')`);
    await cdp.waitFor(`document.querySelector('.sidebar .nav-item[aria-current="page"]')?.dataset.page==='diagnostics'`, 5_000, 'diagnostics after confirmed model discard');
    result.checks.push({ id: 'model-dirty-navigation-cancel-and-confirm', status: 'passed', evidence: { dialog: modelGuard.text, draftRetainedOnCancel: true } });

    const alerts = await cdp.evaluate(`(()=>{const shown=n=>!!n&&(n.offsetWidth||n.offsetHeight||n.getClientRects().length);return [...document.querySelectorAll('[role="alert"]')].filter(shown).map(n=>n.innerText.trim()).filter(Boolean)})()`);
    assert.deepEqual(alerts, [], 'global error alert is unexpectedly visible');
    result.checks.push({ id: 'global-error-state-readable', status: 'passed', evidence: { visibleAlerts: alerts } });
    const quit = await cdp.evaluate(`(()=>{const n=document.querySelector('[data-testid="quit-application"]');return !!n&&(n.offsetWidth||n.offsetHeight||n.getClientRects().length)&&n.disabled===false})()`);
    assert.equal(quit, true, 'visible enabled quit entry not found');
    result.checks.push({ id: 'global-quit-entry-reachable', status: 'passed', evidence: { visible: quit } });
    await cdp.clickElement('[data-testid="quit-application"]');
    const confirmations = [];
    const quitDeadline = Date.now() + 20_000;
    while (Date.now() < quitDeadline && child.exitCode === null) {
      const quitDialog = await cdp.evaluate(`document.querySelector('[role="dialog"],[role="alertdialog"]')?.innerText ?? ''`).catch(() => '');
      if (quitDialog) {
        assert.match(quitDialog, /退出|关闭|未保存|草稿/);
        const action = await cdp.evaluate(`(()=>{const d=document.querySelector('[role="dialog"],[role="alertdialog"]');return [...(d?.querySelectorAll('button')??[])].find(n=>/退出|确认|丢弃|关闭/.test(n.textContent))?.textContent.trim()??null})()`);
        assert.ok(action, `quit confirmation has no confirm action: ${quitDialog}`);
        confirmations.push({ text: quitDialog, action });
        try {
          await cdp.clickElement(`Array.from(document.querySelectorAll('[role="dialog"] button,[role="alertdialog"] button')).find(n=>n.textContent.trim()===${JSON.stringify(action)})`);
        } catch (error) {
          if (child.exitCode === null) {
            try { await waitForExit(child, 5_000); }
            catch { throw error; }
          }
          break;
        }
        const stageDeadline = Date.now() + 2_500;
        while (Date.now() < stageDeadline && child.exitCode === null) {
          const nextDialog = await cdp.evaluate(`document.querySelector('[role="dialog"],[role="alertdialog"]')?.innerText ?? ''`).catch(() => '');
          if (!nextDialog || nextDialog !== quitDialog) break;
          await sleep(100);
        }
      } else {
        await sleep(150);
      }
    }
    const exit = await waitForExit(child, 20_000);
    assert.deepEqual(exit, { code: 0, signal: null }, `quit button did not exit normally: ${JSON.stringify(exit)}`);
    result.normalExit = { route: 'visible quit button', confirmations, exit };
    result.checks.push({ id: 'normal-exit-code-zero', status: 'passed', evidence: result.normalExit });
    result.status = 'passed';
  } catch (error) {
    result.error = error.stack ?? String(error);
    if (spawnError) result.spawnError = spawnError.message;
    result.status = 'failed';
  } finally {
    cdp?.close();
    if (child.pid && child.exitCode === null) {
      try {
        const identity = await native(['-Action', 'process-info', '-RootPid', String(child.pid)]);
        const verified = identity.ok === true && identity.pid === child.pid
          && path.resolve(identity.executablePath).toLowerCase() === path.resolve(binary).toLowerCase()
          && String(identity.commandLine).includes(dataDir);
        result.cleanup.identity = { ...identity, verified };
        if (!verified) throw new Error(`refusing forced cleanup: process identity mismatch: ${JSON.stringify(identity)}`);
        child.kill();
        result.cleanup.forced = true;
        result.cleanup.exit = await waitForExit(child, 10_000);
      } catch (error) {
        result.cleanup.error = error.message;
        result.status = 'failed';
      }
    } else {
      result.cleanup.exit = { code: child.exitCode, signal: child.signalCode };
      result.cleanup.forced = false;
    }
    result.finishedAt = new Date().toISOString();
    result.logs = logs;
    await fs.mkdir(runRoot, { recursive: true });
    await fs.writeFile(path.join(runRoot, 'result.json'), `${JSON.stringify(result, null, 2)}\n`, 'utf8');
    console.log(`ui navigation report: ${path.join(runRoot, 'result.json')}`);
  }
  if (result.status !== 'passed') process.exitCode = 1;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main();
