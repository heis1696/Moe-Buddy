import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { access, mkdir, mkdtemp, readFile, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { basename, dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as delay } from 'node:timers/promises';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const output = join(root, 'target', 'chat-u2-browser');
const executable = process.env.MOE_CHAT_BINARY || join(root, 'target', 'debug', process.platform === 'win32' ? 'moe-chat-host.exe' : 'moe-chat-host');
const browserCandidates = [
  process.env.MOE_BROWSER,
  process.platform === 'win32' && join(process.env['PROGRAMFILES(X86)'] || '', 'Microsoft', 'Edge', 'Application', 'msedge.exe'),
  process.platform === 'win32' && join(process.env.PROGRAMFILES || '', 'Microsoft', 'Edge', 'Application', 'msedge.exe'),
  '/usr/bin/chromium',
  '/usr/bin/google-chrome',
].filter(Boolean);

async function findBrowser() {
  for (const candidate of browserCandidates) {
    try { await access(candidate); return candidate; } catch {}
  }
  throw new Error('No browser found. Set MOE_BROWSER to an Edge/Chromium executable.');
}

async function waitFor(read, label, timeout = 15_000) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const value = await read();
    if (value) return value;
    await delay(100);
  }
  throw new Error(`Timed out: ${label}`);
}

async function stop(child) {
  if (!child || child.exitCode !== null || child.signalCode !== null) return;
  if (process.platform === 'win32' && Number.isInteger(child.pid) && child.pid > 0) {
    const killer = spawn('taskkill', ['/PID', String(child.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
    await new Promise((done) => { killer.once('exit', done); killer.once('error', done); });
  } else {
    child.kill();
  }
}

class DevTools {
  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    this.exceptions = [];
    socket.addEventListener('message', ({ data }) => {
      const message = JSON.parse(data);
      if (message.method === 'Runtime.exceptionThrown') this.exceptions.push(message.params.exceptionDetails.text);
      const pending = this.pending.get(message.id);
      if (!pending) return;
      this.pending.delete(message.id);
      clearTimeout(pending.timeout);
      if (message.error) pending.reject(new Error(message.error.message));
      else pending.resolve(message.result);
    });
  }

  call(method, params = {}) {
    const id = this.nextId++;
    return new Promise((resolveResult, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`DevTools deadline: ${method}`));
      }, 20_000);
      this.pending.set(id, { resolve: resolveResult, reject, timeout });
      this.socket.send(JSON.stringify({ id, method, params }));
    });
  }

  async evaluate(expression) {
    const result = await this.call('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true });
    assert.ok(!result.exceptionDetails, `Browser evaluation failed: ${result.exceptionDetails?.text}`);
    return result.result.value;
  }

  close() {
    for (const pending of this.pending.values()) {
      clearTimeout(pending.timeout);
      pending.reject(new Error('DevTools closed'));
    }
    this.pending.clear();
    this.socket.close();
  }
}

let host;
let browser;
let devtools;
let profile;
const checks = [];

try {
  await access(executable);
  const browserExecutable = await findBrowser();
  await mkdir(output, { recursive: true });
  profile = await mkdtemp(join(tmpdir(), 'moe-chat-browser-'));
  host = spawn(executable, [], { cwd: root, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  let hostOutput = '';
  host.stdout.setEncoding('utf8');
  host.stdout.on('data', (chunk) => { hostOutput += chunk; });
  host.on('error', (error) => { hostOutput = `host startup failed: ${error.code}`; });
  const origin = await waitFor(() => hostOutput.match(/http:\/\/127\.0\.0\.1:\d+/)?.[0], 'host startup');
  browser = spawn(browserExecutable, [
    '--headless=new', '--disable-gpu', '--no-first-run', '--no-default-browser-check',
    '--remote-debugging-port=0', '--remote-debugging-address=127.0.0.1',
    `--user-data-dir=${profile}`, 'about:blank',
  ], { windowsHide: true, stdio: 'ignore' });
  let browserFailure;
  browser.on('error', (error) => { browserFailure = error; });
  const debugPort = await waitFor(async () => {
    if (browserFailure) throw browserFailure;
    try { return Number((await readFile(join(profile, 'DevToolsActivePort'), 'utf8')).split('\n')[0]); } catch { return null; }
  }, 'browser startup');
  const targets = await (await fetch(`http://127.0.0.1:${debugPort}/json/list`)).json();
  const target = targets.find((entry) => entry.type === 'page');
  assert.ok(target, 'browser has a page');
  const socket = new WebSocket(target.webSocketDebuggerUrl);
  await new Promise((done, reject) => { socket.addEventListener('open', done, { once: true }); socket.addEventListener('error', reject, { once: true }); });
  devtools = new DevTools(socket);
  await devtools.call('Runtime.enable');
  await devtools.call('Page.enable');
  await devtools.call('Emulation.setDeviceMetricsOverride', { width: 1440, height: 960, deviceScaleFactor: 1, mobile: false });
  await devtools.call('Page.navigate', { url: origin });
  const evaluate = (expression) => devtools.evaluate(expression);
  const waitUi = (expression, label) => waitFor(() => evaluate(expression), label);
  const click = (selector) => evaluate(`document.querySelector(${JSON.stringify(selector)}).click()`);
  const fill = (selector, text) => evaluate(`(() => { const input = document.querySelector(${JSON.stringify(selector)}); input.value = ${JSON.stringify(text)}; input.dispatchEvent(new Event('input', {bubbles:true})); })()`);
  const snapshot = () => evaluate("fetch('/api/state', {cache:'no-store'}).then(response => response.json()).then(body => body.snapshot)");
  const checked = (label) => { checks.push(label); console.log(`PASS: ${label}`); };
  await waitUi("document.querySelector('#chat-input') && !document.querySelector('#chat-input').disabled", 'real bootstrap and chat mount');
  assert.equal((await snapshot()).mode, 'offline');
  checked('real host bootstrap and UiManager-mounted chat');

  await fill('#chat-input', '闭环验收问候');
  await evaluate("document.querySelector('#chat-input').dispatchEvent(new KeyboardEvent('keydown', {key:'Enter',isComposing:true,bubbles:true,cancelable:true}))");
  await delay(250);
  assert.equal((await snapshot()).chat.messages.length, 0);
  await evaluate("document.querySelector('#chat-input').dispatchEvent(new KeyboardEvent('keydown', {key:'Enter',shiftKey:true,bubbles:true,cancelable:true}))");
  await delay(250);
  assert.equal((await snapshot()).chat.messages.length, 0);
  await click('#send-button');
  await waitUi("document.querySelectorAll('.message').length === 2 && !document.querySelector('#chat-input').disabled", 'first chat response');
  assert.match((await snapshot()).chat.messages[1].text, /离线验收/);
  checked('IME and Shift+Enter do not send; real chat workflow delivers');

  await click('[data-panel-nav="memory"]');
  await waitUi("!document.querySelector('#memory-input').disabled", 'memory mount');
  await fill('#memory-input', '闭环记忆：喜欢茉莉花茶');
  await click('#remember-button');
  await waitUi("document.querySelector('#memory-list').textContent.includes('茉莉花茶') && !document.querySelector('#memory-input').disabled", 'authorized memory write');
  await fill('#chat-input', '下一轮记忆召回验收');
  await click('#send-button');
  await waitUi("document.querySelectorAll('.message').length === 4 && !document.querySelector('#chat-input').disabled", 'memory recall delivery');
  assert.match((await snapshot()).chat.messages[3].text, /茉莉花茶/);
  checked('explicit private memory write and next-turn authorized recall');

  await click('[data-panel-nav="presets"]');
  await waitUi("document.querySelector('[data-preset-id=\"offline-reflect\"]') && !document.querySelector('[data-preset-id=\"offline-reflect\"]').disabled", 'preset mount');
  await click('[data-preset-id="offline-reflect"]');
  await waitFor(async () => (await snapshot()).active_preset === 'offline-reflect', 'real preset rebinding');
  await waitUi("!document.querySelector('#chat-input').disabled", 'preset command completion');
  checked('actual PresetRegistry binding changes');

  await click('[data-panel-nav="diagnostics"]');
  await waitUi("!document.querySelector('#refresh-diagnostics').disabled", 'diagnostics mount');
  await click('#refresh-diagnostics');
  await waitUi("document.querySelector('#diagnostics-list').children.length > 0 && !document.querySelector('#refresh-diagnostics').disabled", 'controlled diagnostic query');
  const diagnostics = (await snapshot()).diagnostics;
  assert.ok(diagnostics.length > 0 && diagnostics.length <= 8);
  assert.ok(!JSON.stringify(diagnostics).includes('茉莉花茶'));
  checked('bounded authorized diagnostics without message or memory bodies');

  await fill('#chat-input', '隐藏和关闭验收草稿');
  await click('[data-panel-hide="chat"]');
  await waitUi("document.querySelector('#panel-chat').hidden && !document.querySelector('[data-panel-nav=\"chat\"]').disabled", 'chat hide');
  let current = await snapshot();
  assert.equal(current.chat.draft, '');
  const hiddenPanel = current.panels.find((panel) => panel.kind === 'chat');
  assert.equal(hiddenPanel.visibility, 'hidden');
  assert.ok(hiddenPanel.unmounts >= 1);
  await click('[data-panel-nav="chat"]');
  await waitUi("!document.querySelector('#chat-input').disabled && document.querySelector('#chat-input').value === '隐藏和关闭验收草稿'", 'hidden draft restoration');
  await click('[data-panel-close="chat"]');
  await waitUi("!document.querySelector('#close-confirmation').hidden && !document.querySelector('#cancel-close').disabled", 'kernel confirmation');
  await click('#cancel-close');
  await waitUi("document.querySelector('#close-confirmation').hidden && !document.querySelector('#chat-input').disabled", 'cancel close');
  assert.equal((await snapshot()).chat.draft, '隐藏和关闭验收草稿');
  await click('[data-panel-close="chat"]');
  await waitUi("!document.querySelector('#discard-close').disabled", 'second close confirmation');
  await click('#discard-close');
  await waitUi("document.querySelector('#panel-chat').hidden && !document.querySelector('[data-panel-nav=\"chat\"]').disabled", 'confirmed close');
  assert.equal((await snapshot()).panels.find((panel) => panel.kind === 'chat').visibility, 'closed');
  await click('[data-panel-nav="chat"]');
  await waitUi("!document.querySelector('#chat-input').disabled", 'chat reopen');
  assert.equal((await snapshot()).chat.draft, '');
  checked('real hide/unmount/restore, kernel close confirmation and reopen');

  await evaluate("window.__draftSyncTexts = []; window.__draftSyncResponses = []; const original = window.fetch; window.fetch = async (...args) => { const action = args[0] === '/api/action' ? JSON.parse(args[1].body) : null; if (action && action.action === 'set_draft') window.__draftSyncTexts.push(action.text); const response = await original(...args); if (action && action.action === 'set_draft') { const body = await response.clone().json(); window.__draftSyncResponses.push({text:action.text,status:response.status,error:body.error?.message || ''}); } return response; };");
  await fill('#chat-input', '已有同步草稿');
  await click('[data-panel-hide="chat"]');
  await waitUi("document.querySelector('#panel-chat').hidden && !document.querySelector('[data-panel-nav=\"chat\"]').disabled", 'sync draft before clearing');
  assert.ok(await evaluate("window.__draftSyncTexts.includes('已有同步草稿')"), 'non-empty draft reaches the real host');
  await click('[data-panel-nav="chat"]');
  await waitUi("!document.querySelector('#chat-input').disabled && document.querySelector('#chat-input').value === '已有同步草稿'", 'reopen synchronized draft');
  await fill('#chat-input', '');
  assert.equal(await evaluate("document.querySelector('#chat-input').value"), '');
  await click('[data-panel-hide="chat"]');
  const emptyDraftResponse = await waitFor(() => evaluate("window.__draftSyncResponses.findLast((response) => response.text === '') || null"), 'real host empty draft response');
  assert.equal(emptyDraftResponse.error, '', `real host rejected empty draft with HTTP ${emptyDraftResponse.status}: ${emptyDraftResponse.error}`);
  assert.equal(emptyDraftResponse.status, 200, 'real host accepts synchronized empty draft');
  await waitUi("document.querySelector('#panel-chat').hidden && !document.querySelector('[data-panel-nav=\"chat\"]').disabled", 'hide after empty draft sync');
  assert.ok(await evaluate("window.__draftSyncTexts.includes('')"), 'empty draft reaches the real host');
  await click('[data-panel-nav="chat"]');
  await waitUi("!document.querySelector('#chat-input').disabled && document.querySelector('#chat-input').value === ''", 'empty draft stays empty after reopen');
  assert.equal((await snapshot()).chat.draft, '');
  await click('[data-panel-close="chat"]');
  await waitUi("document.querySelector('#panel-chat').hidden && document.querySelector('#close-confirmation').hidden && !document.querySelector('[data-panel-nav=\"chat\"]').disabled", 'direct close without draft confirmation');
  current = await snapshot();
  assert.equal(current.panels.find((panel) => panel.kind === 'chat').visibility, 'closed');
  await click('[data-panel-nav="chat"]');
  await waitUi("!document.querySelector('#chat-input').disabled && document.querySelector('#chat-input').value === ''", 'reopen after direct close');
  assert.equal((await snapshot()).chat.draft, '');
  checked('synced draft deletion stays empty through hide, direct close and reopen');

  const desktop = await devtools.call('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
  await writeFile(join(output, 'desktop.png'), Buffer.from(desktop.data, 'base64'));
  await devtools.call('Emulation.setDeviceMetricsOverride', { width: 390, height: 844, deviceScaleFactor: 1, mobile: true });
  await delay(200);
  assert.ok(await evaluate('document.documentElement.scrollWidth <= innerWidth'), 'mobile layout has no horizontal overflow');
  const mobile = await devtools.call('Page.captureScreenshot', { format: 'png', captureBeyondViewport: false });
  await writeFile(join(output, 'mobile.png'), Buffer.from(mobile.data, 'base64'));
  checked('desktop/mobile screenshots and narrow-screen layout');

  await evaluate("(() => {const original = window.fetch; window.fetch = async (...args) => {const response = await original(...args); if (args[0] === '/api/action' && JSON.parse(args[1].body).action === 'send') {window.fetch = original; throw new TypeError('synthetic acknowledgement loss');} return response;};})()");
  await fill('#chat-input', '回执丢失验收');
  await click('#send-button');
  await waitUi("!document.querySelector('#unknown-banner').hidden", 'unknown operation banner');
  assert.ok(await evaluate("document.querySelector('#send-button').disabled"));
  current = await snapshot();
  assert.equal(current.chat.messages.length, 6);
  assert.match(current.chat.messages[5].text, /offline-reflect/);
  await delay(350);
  assert.equal((await snapshot()).chat.messages.length, 6);
  await click('#refresh-state');
  await waitUi("document.querySelector('#unknown-banner').hidden && !document.querySelector('#chat-input').disabled", 'safe state reconciliation');
  assert.equal((await snapshot()).chat.messages.length, 6);
  checked('lost acknowledgement does not resend; read-only refresh reconciles');

  await fill('#chat-input', '撤权前未同步聊天草稿');
  await fill('#memory-input', '撤权前未提交记忆文本');
  assert.deepEqual(await evaluate("({chat:document.querySelector('#chat-input').value,memory:document.querySelector('#memory-input').value})"), { chat: '撤权前未同步聊天草稿', memory: '撤权前未提交记忆文本' });
  const revokeStatus = await evaluate("(async () => {const bootstrap = await (await fetch('/api/bootstrap')).json(); return (await fetch('/api/action', {method:'POST',headers:{'Content-Type':'application/json','X-CSRF-Token':bootstrap.csrf},body:JSON.stringify({id:crypto.randomUUID(),action:'revoke'})})).status;})()");
  assert.equal(revokeStatus, 200, 'real host accepts revocation');
  await click('#refresh-state');
  await waitUi("document.querySelector('#chat-input').value === '' && document.querySelector('#memory-input').value === '' && document.querySelector('#chat-input').disabled && document.querySelector('#memory-input').disabled && document.querySelector('#panel-chat').hidden", 'revoked text inputs cleared');
  current = await snapshot();
  assert.ok(current.revoked);
  assert.equal(current.chat.draft, '');
  assert.equal(current.chat.messages.length, 0);
  assert.equal(current.memories.length, 0);
  assert.equal(current.diagnostics.length, 0);
  assert.ok(current.panels.every((panel) => panel.visibility === 'closed'));
  assert.ok(await evaluate("document.querySelectorAll('#message-list .message, #memory-list .record-item, #diagnostics-list .record-item, #preset-list .preset-option').length === 0"), 'protected DOM caches are removed, not merely hidden');
  assert.ok(await evaluate("Array.from(document.querySelectorAll('.panel')).every((panel) => panel.hidden)"));
  assert.ok(await evaluate("Array.from(document.querySelectorAll('[data-panel-nav], [data-panel-hide], [data-panel-close], #send-button, #remember-button, #refresh-memories, #refresh-diagnostics')).every((button) => button.disabled)"), 'mutating operations are disabled');
  checked('real revocation clears unsynced inputs, protected DOM/snapshot caches and disables operations');
  assert.deepEqual(devtools.exceptions, [], 'no uncaught browser exceptions');
  await writeFile(join(output, 'result.json'), JSON.stringify({ mode: 'real-loopback-offline', checks, screenshots: ['desktop.png', 'mobile.png'] }, null, 2) + '\n');
  console.log(`Artifacts: ${output}`);
} finally {
  devtools?.close();
  await stop(browser);
  await stop(host);
  if (profile) {
    const actual = await realpath(profile);
    const parent = await realpath(tmpdir());
    assert.equal(dirname(actual).toLowerCase(), parent.toLowerCase());
    assert.ok(basename(actual).startsWith('moe-chat-browser-'));
    await rm(actual, { recursive: true, force: true, maxRetries: 3, retryDelay: 200 });
  }
}
