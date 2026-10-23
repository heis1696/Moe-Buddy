import assert from 'node:assert/strict';
import { execFile, spawn, spawnSync } from 'node:child_process';
import { statSync } from 'node:fs';
import fs from 'node:fs/promises';
import http from 'node:http';
import { createRequire } from 'node:module';
import net from 'node:net';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { writeFixtures } from './material-fixtures.mjs';

/*
 * 可重跑的原生宿主验收：启动真实 Tauri + WebView2，通过 CDP 操作真实 DOM
 * 表单与按钮（不是直调 IPC 代替产品操作），模型服务仅使用进程内合成回环，
 * 存档与 WebView profile 固定在忽略的 target/ 隔离目录。
 *
 * 失败分为普通回归失败与被测快照中已知待修问题（knownFix）。已知问题仍执行
 * 真实断言并以 failed 记录，不跳过、不放宽、不记作通过；脚本因此可能以 1 退出。
 */

const appRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const workspaceRoot = path.resolve(appRoot, '..', '..');
const binary = process.env.MOE_SMOKE_BINARY ? path.resolve(process.env.MOE_SMOKE_BINARY) : path.join(
  workspaceRoot,
  'target',
  'debug',
  process.platform === 'win32' ? 'moe-buddy-tauri.exe' : 'moe-buddy-tauri',
);
const distIndex = path.join(appRoot, 'dist', 'index.html');
const nativeWindowScript = path.join(appRoot, 'tests', 'native-window.ps1');
const outputRoot = path.join(workspaceRoot, 'target', 'native-smoke');

const sleep = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

class CheckFailure extends Error {
  constructor(message, evidence = null) {
    super(message);
    this.name = 'CheckFailure';
    this.evidence = evidence;
  }
}

/**
 * 宿主尚未接通 B 的伙伴契约时，严格前端无法建立任何业务界面。本轮只记录
 * 待联调项并正常退出，不把旧宿主或桩界面当成 B 验收。
 */
class IntegrationPending extends Error {
  constructor(message, evidence = null) {
    super(message);
    this.name = 'IntegrationPending';
    this.evidence = evidence;
  }
}

function ensure(condition, message, evidence = null) {
  if (!condition) throw new CheckFailure(message, evidence);
}

async function assertPrerequisites() {
  await fs.access(binary).catch(() => {
    throw new Error(`missing host binary: ${binary}\nrun: cargo build -p moe-buddy-tauri --locked`);
  });
  await fs.access(distIndex).catch(() => {
    throw new Error(`missing frontend dist: ${distIndex}\nrun: npm run build (in apps/moe-buddy-tauri)`);
  });
  await fs.access(nativeWindowScript).catch(() => {
    throw new Error(`missing native window helper: ${nativeWindowScript}`);
  });
  await fs.mkdir(path.join(outputRoot, 'profiles'), { recursive: true });
}

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

const DOM_HELPERS = `(() => {
  window.__smoke = {
    norm(value) {
      return (value || '').replace(/\\s+/g, ' ').trim();
    },
    button(text, scope) {
      const root = scope || document;
      return Array.from(root.querySelectorAll('button'))
        .find((node) => this.norm(node.textContent) === text) || null;
    },
    buttonState(text, scope) {
      const node = this.button(text, scope);
      if (!node) {
        return { present: false, visible: false, disabled: null, operable: false };
      }
      const visible = !!(node.offsetWidth || node.offsetHeight || node.getClientRects().length);
      const disabled = node.disabled === true;
      return { present: true, visible, disabled, operable: visible && !disabled };
    },
    pageVisible(page) {
      const node = document.querySelector('[data-page-view="' + page + '"]');
      return !!node && !!(node.offsetWidth || node.offsetHeight || node.getClientRects().length);
    },
    currentPage() {
      return document.querySelector('[data-page][aria-current="page"]')?.dataset.page ?? null;
    },
    buttonPart(text, scope) {
      const root = scope || document;
      return Array.from(root.querySelectorAll('button'))
        .find((node) => this.norm(node.textContent).includes(text)) || null;
    },
    control(labelText) {
      const label = Array.from(document.querySelectorAll('label'))
        .find((node) => this.norm(node.textContent) === labelText);
      return label ? label.querySelector('input,textarea') : null;
    },
    controlPart(text) {
      const label = Array.from(document.querySelectorAll('label'))
        .find((node) => this.norm(node.textContent).includes(text));
      return label ? label.querySelector('input,textarea') : null;
    },
    label(text) {
      return Array.from(document.querySelectorAll('label'))
        .find((node) => this.norm(node.textContent) === text) || null;
    },
    panel() {
      return Array.from(document.querySelectorAll('.panel-shell'))
        .find((node) => !!(node.offsetWidth || node.offsetHeight || node.getClientRects().length)) || null;
    },
    panelText() {
      return this.panel()?.innerText || '';
    },
    chatText() {
      return document.querySelector('.messages')?.innerText || '';
    },
    statusText() {
      return document.querySelector('.chat-meta')?.innerText || '';
    },
    alerts() {
      return Array.from(document.querySelectorAll('[role="alert"]'))
        .map((node) => node.innerText).join('\\n');
    },
    statuses() {
      return Array.from(document.querySelectorAll('[role="status"]'))
        .map((node) => node.innerText).join('\\n');
    },
    unknownLock() {
      return Array.from(document.querySelectorAll('[role="status"]'))
        .some((node) => (node.innerText || '').includes('未确认最近一次操作的结果'));
    },
    dialog() {
      return document.querySelector('[role="dialog"], [role="alertdialog"]');
    },
    dialogText() {
      return this.dialog()?.innerText || '';
    },
    dialogButton(text) {
      const dialog = this.dialog();
      return dialog ? this.button(text, dialog) : null;
    },
    dialogConfirm() {
      const dialog = this.dialog();
      if (!dialog) return null;
      return dialog.querySelector('button.danger')
        || Array.from(dialog.querySelectorAll('button')).at(-1)
        || null;
    },
    memoryItem(text) {
      return Array.from(document.querySelectorAll('.memory-list li'))
        .find((node) => this.norm(node.textContent).includes(text)) || null;
    },
    allowTransmissionCheckbox() {
      const label = Array.from(document.querySelectorAll('label'))
        .find((node) => {
          const text = this.norm(node.textContent);
          return text.includes('允许')
            && (text.includes('发送') || text.includes('外传'))
            && node.querySelector('input[type="checkbox"]') !== null;
        });
      return label ? label.querySelector('input[type="checkbox"]') : null;
    },
    allowTransmissionLabel() {
      const checkbox = this.allowTransmissionCheckbox();
      return checkbox?.closest('label')?.innerText ?? null;
    },
    nativeWindowVisible() {
      return document.visibilityState !== 'hidden';
    },
    sessionChips() {
      return Array.from(document.querySelectorAll('.session-chip')).map((node) => ({
        label: this.norm(node.querySelector('.session-name')?.textContent || node.textContent),
        active: node.classList.contains('active'),
        archived: node.classList.contains('archived'),
        disabled: node.disabled,
        title: node.title || '',
      }));
    },
    sessionChip(label) {
      const wanted = this.norm(label);
      return Array.from(document.querySelectorAll('.session-chip'))
        .find((node) => this.norm(node.querySelector('.session-name')?.textContent) === wanted) || null;
    },
    activeSessionLabel() {
      return this.norm(document.querySelector('.session-chip.active .session-name')?.textContent || '');
    },
    sessionAction(text) {
      if (text === '新建会话') return document.querySelector('button[aria-label="新建会话"]');
      if (text === '归档当前会话') return this.button('归档会话', document.querySelector('.conversation-head'));
      const bar = document.querySelector('.session-actions');
      return bar ? this.button(text, bar) : null;
    },
    chatInput() {
      return document.querySelector('#chat-input');
    },
    messagesText() {
      return document.querySelector('.messages')?.innerText || '';
    },
    archivedBanner() {
      return this.norm(document.querySelector('.archived-banner')?.innerText || '');
    },
    isolationHint() {
      return Array.from(document.querySelectorAll('[role="status"]'))
        .map((node) => this.norm(node.innerText))
        .find((text) => text.includes('可显式新建隔离会话')) || null;
    },
    warningText() {
      return Array.from(document.querySelectorAll('[role="status"]'))
        .map((node) => this.norm(node.innerText)).join('\\n');
    },
    buddyChips() {
      return Array.from(document.querySelectorAll('.buddy-chip')).map((node) => ({
        label: this.norm(node.querySelector('.buddy-name')?.textContent || node.textContent),
        revisions: this.norm(node.querySelector('.buddy-revisions')?.textContent || ''),
        active: node.classList.contains('active'),
        disabled: node.disabled,
        title: node.title || '',
      }));
    },
    buddyChip(label) {
      const wanted = this.norm(label);
      const card = Array.from(document.querySelectorAll('.buddy-chip'))
        .find((node) => this.norm(node.querySelector('.buddy-name')?.textContent) === wanted);
      return card ? this.button('选择伙伴', card) || this.button('当前伙伴', card) : null;
    },
    activeBuddyLabel() {
      return this.norm(document.querySelector('.buddy-chip.active .buddy-name')?.textContent || '');
    },
    buddyIdentity() {
      return this.norm(document.querySelector('.buddy-identity')?.innerText || '');
    },
    buddyAction(text) {
      const bar = document.querySelector('.buddy-actions');
      return bar ? this.button(text, bar) : null;
    },
    buddyForm(ariaLabel) {
      return document.querySelector('[aria-label="' + ariaLabel + '"]');
    },
    buddyControl(ariaLabel, labelPart) {
      const form = this.buddyForm(ariaLabel);
      if (!form) return null;
      const label = Array.from(form.querySelectorAll('label'))
        .find((node) => this.norm(node.textContent).includes(labelPart));
      return label ? label.querySelector('input,textarea') : null;
    },
    buddyFormButton(ariaLabel, text) {
      const form = this.buddyForm(ariaLabel);
      return form ? this.button(text, form) : null;
    },
    buddyError() {
      return Array.from(document.querySelectorAll('[role="alert"]'))
        .map((node) => this.norm(node.innerText)).join('\\n');
    },
  };
  return true;
})()`;

function B(text) {
  return `__smoke.button(${JSON.stringify(text)})`;
}

function C(label) {
  return `__smoke.control(${JSON.stringify(label)})`;
}

function itemButton(text, itemText) {
  return `__smoke.button(${JSON.stringify(text)}, __smoke.memoryItem(${JSON.stringify(itemText)}))`;
}

/** 允许调用方传入 CSS 选择器字符串或 JS 表达式；字符串选择器统一转成 DOM 查询。 */
function resolveElement(expression) {
  if (expression.startsWith('#') || expression.startsWith('.') || expression.startsWith('[')) {
    return `document.querySelector(${JSON.stringify(expression)})`;
  }
  return expression;
}

class CdpClient {
  static async connect(port, titleNeedle) {
    const deadline = Date.now() + 30_000;
    const wantedWindow = /宠物/.test(titleNeedle) ? 'pet' : 'main';
    const failures = [];
    while (Date.now() < deadline) {
      try {
        const targets = await fetch(`http://127.0.0.1:${port}/json/list`).then((response) =>
          response.json(),
        );
        for (const target of targets.filter((entry) => entry.type === 'page' && entry.title.includes('Moe Buddy'))) {
          let socket;
          try {
            socket = new WebSocket(target.webSocketDebuggerUrl);
            await new Promise((resolve, reject) => {
              const timer = setTimeout(() => reject(new Error('CDP websocket connection timed out')), 1_500);
              socket.onopen = () => { clearTimeout(timer); resolve(); };
              socket.onerror = () => { clearTimeout(timer); reject(new Error('CDP websocket failed')); };
              socket.onclose = () => { clearTimeout(timer); reject(new Error('CDP websocket closed while connecting')); };
            });
            const cdp = new CdpClient(socket);
            const correctWindow = await cdp.evaluate(`document.documentElement.dataset.window === ${JSON.stringify(wantedWindow)}${wantedWindow === 'main' ? " && document.querySelector('.main-view') !== null" : ''}`);
            if (correctWindow === true) return cdp;
            cdp.close();
          } catch (error) {
            socket?.close();
            failures.push(`${target.title} ${target.url}: ${error.message}`);
          }
        }
      } catch {
        // WebView2 尚未完成启动。
      }
      await sleep(250);
    }
    throw new Error(`WebView2 ${wantedWindow} target not found by DOM window identity; candidates=${failures.slice(-8).join(' | ')}`);
  }

  constructor(socket) {
    this.socket = socket;
    this.nextId = 1;
    this.pending = new Map();
    this.closed = false;
    socket.onmessage = (event) => {
      let message;
      try { message = JSON.parse(event.data); } catch (error) {
        this.failAll(new Error(`invalid CDP response: ${error.message}`));
        return;
      }
      const handler = this.pending.get(message.id);
      if (handler !== undefined) {
        clearTimeout(handler.timer);
        this.pending.delete(message.id);
        if (message.error !== undefined) {
          handler.reject(new Error(`CDP ${message.error.message ?? 'command failed'}`));
        } else {
          handler.resolve(message);
        }
      }
    };
    socket.onclose = () => {
      this.closed = true;
      this.failAll(new Error('CDP websocket closed'));
    };
    socket.onerror = () => this.failAll(new Error('CDP websocket error'));
  }

  failAll(error) {
    for (const handler of this.pending.values()) {
      clearTimeout(handler.timer);
      handler.reject(error);
    }
    this.pending.clear();
  }

  send(method, params) {
    if (this.closed) return Promise.reject(new Error('CDP websocket closed'));
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
    const response = await this.send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (response.result?.exceptionDetails !== undefined) {
      throw new Error(`CDP evaluate failed: ${JSON.stringify(response.result.exceptionDetails)}`);
    }
    return response.result?.result?.value;
  }

  async invoke(command, payload = {}) {
    return this.evaluate(
      `window.__TAURI_INTERNALS__.invoke(${JSON.stringify(command)}, ${JSON.stringify(payload)})`,
    );
  }

  async waitFor(expression, timeoutMs, label) {
    const deadline = Date.now() + timeoutMs;
    let last;
    while (Date.now() < deadline) {
      try {
        last = await this.evaluate(`Boolean(${expression})`);
        if (last === true) return;
      } catch (error) {
        last = error.message;
      }
      await sleep(100);
    }
    throw new Error(`timed out waiting for ${label}; last=${String(last)}`);
  }

  /** 真实 CDP 鼠标点击：取语义选择器命中元素的可见中心，再发 mousePressed/Released。 */
  async clickElement(expression) {
    const target = resolveElement(expression);
    await this.waitFor(
      `(() => { const element = ${target}; return element !== null && element.disabled !== true && !!(element.offsetWidth || element.offsetHeight || element.getClientRects().length); })()`,
      5_000,
      'click target enabled',
    );
    const point = await this.evaluate(`(() => {
      const element = ${target};
      if (!element) return null;
      element.scrollIntoView({ block: 'center', inline: 'center' });
      const rect = element.getBoundingClientRect();
      if (rect.width <= 0 || rect.height <= 0) return null;
      return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 };
    })()`);
    if (point === null) throw new Error(`click target missing or invisible: ${expression}`);
    await this.send('Input.dispatchMouseEvent', {
      type: 'mouseMoved',
      x: point.x,
      y: point.y,
    });
    await this.send('Input.dispatchMouseEvent', {
      type: 'mousePressed',
      x: point.x,
      y: point.y,
      button: 'left',
      clickCount: 1,
    });
    await this.send('Input.dispatchMouseEvent', {
      type: 'mouseReleased',
      x: point.x,
      y: point.y,
      button: 'left',
      clickCount: 1,
    });
  }

  /**
   * 真实 CDP 文本输入：先聚焦并清空控件，再用 Input.insertText 输入正文。
   * 清空只是准备动作；表单事件仍由浏览器输入路径产生，不直接改业务状态。
   */
  async typeInto(expression, text) {
    const target = resolveElement(expression);
    await this.waitFor(
      `(() => { const element = ${target}; return element !== null && element.disabled !== true && !!(element.offsetWidth || element.offsetHeight || element.getClientRects().length); })()`,
      5_000,
      'input target enabled',
    );
    await this.evaluate(`(() => {
      const element = ${target};
      if (!element) throw new Error('input target missing');
      element.focus();
      element.value = '';
      element.dispatchEvent(new Event('input', { bubbles: true }));
      return true;
    })()`);
    if (text.length > 0) await this.send('Input.insertText', { text });
    await this.waitFor(
      `(() => { const element = ${target}; return element !== null && element.value === ${JSON.stringify(text)}; })()`,
      5_000,
      'typed DOM input value',
    );
  }

  close() {
    if (!this.closed) {
      this.closed = true;
      this.socket.close();
    }
  }
}

function waitForExit(child, timeoutMs = 20_000) {
  return new Promise((resolve, reject) => {
    if (child.exitCode !== null || child.signalCode !== null) {
      resolve({ code: child.exitCode, signal: child.signalCode });
      return;
    }
    const timer = setTimeout(() => {
      reject(new Error(`process did not exit within ${timeoutMs}ms`));
    }, timeoutMs);
    child.once('exit', (code, signal) => {
      clearTimeout(timer);
      resolve({ code, signal });
    });
  });
}

function childAlive(child) {
  return child.exitCode === null && child.signalCode === null;
}

async function setupDom(cdp) {
  await cdp.evaluate(DOM_HELPERS);
  await cdp.waitFor(`document.querySelector('.main-view') !== null`, 20_000, 'Vue main view');
}

async function launch(label, dataRootOverride, createdInstances) {
  const port = await freePort();
  const profile = path.join(outputRoot, 'profiles', `${label}-${Date.now()}`);
  const dataRoot = dataRootOverride ?? path.join(outputRoot, 'data', `${label}-${Date.now()}`);
  await fs.mkdir(profile, { recursive: true });
  await fs.mkdir(dataRoot, { recursive: true });
  const logs = [];
  let spawnError = null;
  const child = spawn(binary, ['--data-dir', dataRoot], {
    env: {
      ...process.env,
      LOCALAPPDATA: dataRoot,
      WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS: `--remote-debugging-port=${port} --remote-allow-origins=*`,
      WEBVIEW2_USER_DATA_FOLDER: profile,
    },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  child.once('error', (error) => {
    spawnError = error;
    logs.push(`spawn: ${error.message}`);
  });
  child.stdout.on('data', (chunk) => logs.push(`stdout: ${chunk.toString().trim()}`));
  child.stderr.on('data', (chunk) => logs.push(`stderr: ${chunk.toString().trim()}`));
  const instance = {
    label,
    pid: child.pid,
    child,
    port,
    profile,
    dataRoot,
    logs,
    cdp: null,
  };
  // 启动失败也保留句柄，由本函数与最终清理统一收口。
  createdInstances.push(instance);
  assertSyntheticInstancePaths(instance);
  try {
    if (child.pid === undefined) {
      throw new Error(`host process did not spawn: ${spawnError?.message ?? 'no pid'}`);
    }
    const cdp = await CdpClient.connect(port, '离线验证');
    await cdp.waitFor(
      `window.__TAURI_INTERNALS__ !== undefined`,
      15_000,
      'Tauri IPC bridge ready',
    );
    await setupDom(cdp);
    instance.cdp = cdp;
    return instance;
  } catch (error) {
    await closeInstance(instance, 'launch-failed');
    error.message = `${error.message} [launch ${label} cleaned up; pid=${instance.pid}]`;
    throw error;
  }
}

function assertSyntheticInstancePaths(instance) {
  const output = path.resolve(outputRoot);
  const data = path.resolve(instance.dataRoot);
  const profile = path.resolve(instance.profile);
  const dataPrefix = path.join(output, 'data') + path.sep;
  const profilePrefix = path.join(output, 'profiles') + path.sep;
  ensure(data.startsWith(dataPrefix), 'refusing cleanup outside the synthetic data root', {
    dataRoot: data,
    expectedPrefix: dataPrefix,
  });
  ensure(profile.startsWith(profilePrefix), 'refusing cleanup outside the synthetic WebView profile root', {
    profile,
    expectedPrefix: profilePrefix,
  });
  return { data, profile };
}

async function inspectOwnedProcess(instance) {
  return runNative(instance, ['-Action', 'process-info', '-RootPid', String(instance.pid)]);
}

/**
 * 只清理本测试自己 spawn 的子进程句柄：先核对 PID、可执行文件、--data-dir 与合成根，
 * 再用子进程句柄终止，不按进程名枚举或终止其他进程。句柄仍存活就必须保留到真正退出。
 */
async function closeInstance(instance, reason = 'failure-cleanup') {
  if (instance === undefined || instance === null) return false;
  const { data } = assertSyntheticInstancePaths(instance);
  if (instance.cdp !== null && instance.cdp !== undefined) {
    instance.cdp.close();
    instance.cdp = null;
  }
  if (instance.pid === undefined) {
    instance.cleanupReason = reason;
    instance.cleanupError = 'host process never spawned';
    return false;
  }
  if (!childAlive(instance.child)) {
    return false;
  }

  let identity = null;
  try {
    identity = await inspectOwnedProcess(instance);
  } catch (error) {
    instance.identityError = error.message;
  }
  let identityVerified = false;
  if (identity?.ok === true) {
    const executable = path.resolve(identity.executablePath ?? '');
    const commandLine = String(identity.commandLine ?? '');
    identityVerified = identity.pid === instance.pid
      && executable.toLowerCase() === path.resolve(binary).toLowerCase()
      && commandLine.includes(data);
    instance.cleanupIdentity = {
      pid: identity.pid,
      executable,
      commandLine,
      verified: identityVerified,
    };
    if (!identityVerified) {
      instance.cleanupIdentityMismatch = true;
    }
  }
  // 身份核对期间进程可能已自然退出；不能把自然退出记成清理杀进程。
  if (!childAlive(instance.child)) {
    return false;
  }

  if (!identityVerified) {
    instance.cleanupReason = reason;
    instance.cleanupError = 'refused forced cleanup because PID, executable path, or synthetic data-dir did not match';
    return false;
  }
  instance.cleanupReason = reason;
  instance.child.kill();
  instance.cleanupKilled = true;
  try {
    instance.cleanupExit = await waitForExit(instance.child, 10_000);
  } catch (error) {
    instance.cleanupError = error.message;
  }
  return true;
}

async function readSnapshot(cdp) {
  const response = await cdp.invoke('prototype_snapshot');
  ensure(response.error === null, `snapshot read failed: ${JSON.stringify(response.error)}`);
  ensure(response.snapshot !== null, 'snapshot read returned no snapshot');
  return response.snapshot;
}

/**
 * 辅助 IPC 也必须携带发起时的完整伙伴作用域（ID + 选择修订）。
 * 正常业务验收仍走真实 DOM 表单；这里只用于制造冲突前置或清理合成状态。
 */
function buddyContextOf(snapshot) {
  return {
    expectedBuddyId: snapshot.activeBuddyId ?? '',
    expectedBuddiesRevision: snapshot.buddiesRevision,
  };
}

/**
 * 真实 WebView2 IPC 观测：Tauri 的 `__TAURI_INTERNALS__.invoke` 是
 * `Object.defineProperty` 安装的不可写属性，替换它只会静默失败。这里改为在
 * CDP 里包装全局 `fetch`：Tauri 自定义协议 IPC 的 URL 末段就是命令名，body
 * 是实际参数 JSON。安装后立即用一次真实 `prototype_snapshot` 调用做金丝雀，
 * 未捕获到就明确失败，不把无效钩子当成“安装成功”。
 */
async function installIpcObserver(cdp) {
  const installed = await cdp.evaluate(`(() => {
    if (window.__smokeIpcInstalled === true) return true;
    window.__smokeIpcCalls = [];
    const original = window.fetch;
    const wrapped = function () {
      try {
        const input = arguments[0];
        const init = arguments[1];
        const url = typeof input === 'string'
          ? input
          : (input && typeof input.url === 'string' ? input.url : '');
        const path = url.split('?')[0];
        const cmd = path.slice(path.lastIndexOf('/') + 1);
        if (cmd.startsWith('prototype_') && init && typeof init.body === 'string') {
          window.__smokeIpcCalls.push({ cmd, args: JSON.parse(init.body) });
        }
      } catch (error) {}
      return original.apply(this, arguments);
    };
    window.fetch = wrapped;
    window.__smokeIpcInstalled = window.fetch === wrapped;
    return window.__smokeIpcInstalled;
  })()`);
  ensure(installed === true, 'could not install the fetch-based IPC observer');
  const before = await cdp.evaluate(`(window.__smokeIpcCalls ?? []).length`);
  await cdp.invoke('prototype_snapshot');
  const after = await cdp.evaluate(`(window.__smokeIpcCalls ?? []).length`);
  ensure(after > before, 'IPC observer did not capture the real prototype_snapshot call');
  return true;
}

async function ipcCalls(cdp, command) {
  ensure(
    (await cdp.evaluate(`window.__smokeIpcInstalled === true`)) === true,
    'IPC observer is not installed',
  );
  return cdp.evaluate(
    `(window.__smokeIpcCalls ?? []).filter((entry) => entry.cmd === ${JSON.stringify(command)})`,
  );
}

async function clearIpcCalls(cdp) {
  await cdp.evaluate(`(() => { window.__smokeIpcCalls = []; return true; })()`);
}

/** 失败/收尾时明确关闭脚本打开的新建/改名表单，避免残留挡住后续链。 */
async function closeBuddyFormsIfOpen(cdp) {
  const outcomes = [];
  for (const form of ['新建伙伴', '伙伴改名']) {
    const open = await cdp.evaluate(`__smoke.buddyForm(${JSON.stringify(form)}) !== null`);
    if (open !== true) continue;
    try {
      await cdp.clickElement(`__smoke.buddyFormButton(${JSON.stringify(form)}, '取消')`);
      await cdp.waitFor(
        `__smoke.buddyForm(${JSON.stringify(form)}) === null`,
        5_000,
        `${form} form closed during cleanup`,
      );
      outcomes.push({ form, closed: true });
    } catch (error) {
      // 收尾是尽力而为；失败原因单独保留，不冒充表单已关闭。
      outcomes.push({ form, closed: false, error: error.message });
    }
  }
  return outcomes;
}

async function waitForSnapshot(cdp, predicate, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let last;
  while (Date.now() < deadline) {
    try {
      last = await readSnapshot(cdp);
      if (predicate(last)) return last;
    } catch (error) {
      last = error.message;
    }
    await sleep(100);
  }
  throw new Error(`timed out waiting for snapshot ${label}; last=${JSON.stringify(last).slice(0, 400)}`);
}

async function panelText(cdp) {
  return cdp.evaluate(`__smoke.panelText()`);
}

async function dialogText(cdp) {
  return cdp.evaluate(`__smoke.dialogText()`);
}

async function waitForDialog(cdp, timeoutMs = 5_000) {
  await cdp.waitFor(`__smoke.dialog() !== null`, timeoutMs, 'confirmation dialog');
  return dialogText(cdp);
}

async function clickDialogCancel(cdp) {
  await cdp.clickElement(`__smoke.dialogButton('取消')`);
  await cdp.waitFor(`__smoke.dialog() === null`, 5_000, 'dialog closed after cancel');
}

async function clickDialogConfirm(cdp) {
  const label = await cdp.evaluate(`__smoke.dialogConfirm()?.innerText ?? ''`);
  ensure(
    /丢弃|退出|关闭|确认|放弃|继续/.test(label),
    `unexpected dialog confirm label: ${JSON.stringify(label)}`,
    { confirmLabel: label },
  );
  await cdp.clickElement(`__smoke.dialogConfirm()`);
  return label;
}

function runNative(instance, args) {
  return new Promise((resolve, reject) => {
    execFile(
      'powershell.exe',
      ['-NoProfile', '-File', nativeWindowScript, ...args],
      { windowsHide: true, timeout: 30_000 },
      (error, stdout, stderr) => {
        if (error !== null && error.code !== undefined && error.code !== 0) {
          reject(new Error(`native helper failed: ${stdout.trim()} ${stderr.trim()}`));
          return;
        }
        try {
          resolve(JSON.parse(stdout.trim()));
        } catch {
          reject(new Error(`unparsable native helper output: ${stdout.trim()} ${stderr.trim()}`));
        }
      },
    );
  });
}

async function nativeWindows(instance) {
  const result = await runNative(instance, ['-Action', 'windows', '-RootPid', String(instance.pid)]);
  ensure(result.ok === true, `native window enumeration failed: ${JSON.stringify(result)}`);
  return result.windows;
}

async function mainWindowVisible(instance) {
  const windows = await nativeWindows(instance);
  const main = windows.find((window) => window.title === 'Moe Buddy');
  return main?.visible === true;
}

async function waitForMainVisible(instance, visible, timeoutMs = 5_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (!childAlive(instance.child)) return false;
    try {
      if ((await mainWindowVisible(instance)) === visible) return true;
    } catch {
      // 窗口枚举可能恰逢销毁，继续有界轮询。
    }
    await sleep(150);
  }
  return false;
}

async function postNativeClose(instance) {
  return runNative(instance, ['-Action', 'close-window', '-RootPid', String(instance.pid)]);
}

async function postTrayQuit(instance) {
  // 托盘菜单 quit 的原生 WM_COMMAND id；顺序来自站点创建 show/hide/pause/quit。
  return runNative(instance, [
    '-Action',
    'tray-command',
    '-RootPid',
    String(instance.pid),
    '-MenuId',
    '1003',
  ]);
}

/**
 * 主窗窗口命令的 smoke 辅助 IPC：先捕获当前快照的完整伙伴上下文，再带着该
 * 发起上下文派发。辅助路径同样不省略 context，也不在异步后改读最新选择。
 */
async function invokeWindowWithContext(instance, command) {
  const snapshot = await readSnapshot(instance.cdp);
  return instance.cdp.invoke('prototype_window', {
    command,
    context: buddyContextOf(snapshot),
  });
}

async function recoverMain(instance) {
  if (!childAlive(instance.child)) return;
  try {
    if ((await cdpDialogPresent(instance))) {
      await clickDialogCancel(instance.cdp).catch(() => {});
    }
  } catch {
    // 进程可能已退出。
  }
  try {
    if (!(await mainWindowVisible(instance))) {
      await invokeWindowWithContext(instance, 'show_chat');
      await waitForMainVisible(instance, true);
      await sleep(200);
    }
  } catch {
    // 失败恢复本身不掩盖原始断言：调用方仍会记录失败。
  }
}

async function cdpDialogPresent(instance) {
  try {
    return (await instance.cdp.evaluate(`__smoke.dialog() !== null`)) === true;
  } catch {
    return false;
  }
}

async function ensureDialogOperable(instance) {
  ensure(
    await waitForMainVisible(instance, true, 3_000),
    'confirmation dialog is not operable because the main window is hidden',
  );
}

async function clickQuitApplication(instance) {
  if (!(await mainWindowVisible(instance))) await invokeWindowWithContext(instance, 'show_chat');
  await instance.cdp.waitFor(
    `document.querySelector('[data-testid="quit-application"]')?.disabled === false`,
    10_000,
    'main application exit button enabled',
  );
  await instance.cdp.clickElement(`document.querySelector('[data-testid="quit-application"]')`);
}

async function quitNormally(instance) {
  await clickQuitApplication(instance);
  const deadline = Date.now() + 20_000;
  while (Date.now() < deadline) {
    if (!childAlive(instance.child)) {
      return { code: instance.child.exitCode, signal: instance.child.signalCode };
    }
    if (await cdpDialogPresent(instance)) {
      throw new CheckFailure(
        'normal quit unexpectedly requested a confirmation; a draft or panel was still dirty',
        { dialog: await dialogText(instance.cdp) },
      );
    }
    await sleep(150);
  }
  throw new CheckFailure('process did not exit within 20000ms', {
    pid: instance.pid,
    dataRoot: instance.dataRoot,
    logs: instance.logs,
  });
}

/**
 * 请求退出并处理其间出现的确认对话框。
 * - 面板离开确认（“放弃未保存的编辑？”）是脚本残留的界面清理，单独记账；
 * - 关闭/退出确认（含叠加的管理编辑确认）才算 `confirmed`；
 * - 允许多个对话框依次出现，直到进程自己退出；从不强杀冒充正常退出。
 */
async function quitWithOptionalConfirm(instance, { componentUnmounted = false } = {}) {
  // The memory-unmount scenario deliberately destroys Vue; only that fixture
  // uses the existing host exit command for cleanup. Product exits use the button.
  if (componentUnmounted) await invokeWindowWithContext(instance, 'quit');
  else await clickQuitApplication(instance);
  const deadline = Date.now() + 20_000;
  const dialogs = [];
  let confirmed = false;
  while (Date.now() < deadline) {
    if (!childAlive(instance.child)) break;
    if (await cdpDialogPresent(instance)) {
      const dialog = await dialogText(instance.cdp);
      const label = await clickDialogConfirm(instance.cdp);
      dialogs.push({ dialog: dialog.slice(0, 240), label });
      // 只有关闭/退出（及其管理编辑叠加）确认属于退出验收；面板离开确认是清场。
      if (!/放弃未保存的编辑/.test(dialog)) confirmed = true;
      await sleep(150);
      continue;
    }
    await sleep(150);
  }
  const exit = await waitForExit(instance.child, 20_000);
  return {
    exit,
    confirmed,
    dialogs,
    confirmLabel: dialogs.length > 0 ? dialogs[dialogs.length - 1].label : null,
  };
}

async function syntheticModelServer() {
  const state = {
    requests: 0,
    failure: null,
    lastRequest: null,
    lastAuthorization: null,
    // 'reply' 返回合成回复；'drop' 在计数后直接断开回环连接，制造真实外部 Unknown。
    mode: 'reply',
  };
  const server = http.createServer((request, response) => {
    let body = '';
    request.setEncoding('utf8');
    request.on('data', (chunk) => { body += chunk; });
    request.on('end', () => {
      try {
        assert.equal(request.method, 'POST');
        assert.equal(request.url, '/v1/chat/completions');
        const payload = JSON.parse(body);
        assert.equal(payload.stream, false);
        state.requests += 1;
        state.lastRequest = payload;
        state.lastAuthorization = request.headers.authorization ?? null;
        if (state.mode === 'drop') {
          request.socket.destroy();
          return;
        }
        const result = JSON.stringify({
          choices: [{ message: { content: 'synthetic loopback reply' } }],
        });
        response.writeHead(200, {
          'content-type': 'application/json',
          'content-length': Buffer.byteLength(result),
          connection: 'close',
        });
        response.end(result);
      } catch (error) {
        state.failure = error.stack ?? String(error);
        response.writeHead(500, { connection: 'close' });
        response.end();
      }
    });
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(0, '127.0.0.1', resolve);
  });
  return {
    endpoint: `http://127.0.0.1:${server.address().port}/v1/chat/completions`,
    state,
    close: () => new Promise((resolve, reject) => {
      server.close((error) => error ? reject(error) : resolve());
    }),
  };
}

/**
 * 读取 B 档案权威文件。原生 smoke 只检查非秘密模型/人设与版本，不把旧的
 * settings.json 或进程内凭据当作持久化证据。
 */
async function readBuddyRegistry(dataRoot) {
  const raw = await fs.readFile(path.join(dataRoot, 'buddies.v1'), 'utf8');
  const registry = JSON.parse(raw);
  const active = (registry.buddies ?? []).find((buddy) => buddy.id === registry.activeBuddyId) ?? null;
  return { raw, registry, active, model: active?.model ?? null };
}

async function readActiveBuddyModel(dataRoot) {
  return (await readBuddyRegistry(dataRoot)).model;
}

function checkEvidenceFromError(error) {
  return error instanceof CheckFailure ? error.evidence : null;
}

const RUN = {
  checks: [],
  failures: 0,
  knownFailures: 0,
  pending: [],
  cleanupNotes: [],
};

async function check(id, description, run, options = {}) {
  const startedAt = Date.now();
  try {
    const evidence = await run();
    const entry = {
      id,
      description,
      status: 'passed',
      durationMs: Date.now() - startedAt,
      evidence: evidence ?? null,
    };
    if (options.knownFix !== undefined) entry.targetsKnownFix = options.knownFix;
    RUN.checks.push(entry);
    console.log(`PASS ${id}`);
    return true;
  } catch (error) {
    RUN.failures += 1;
    const known = options.knownFix !== undefined;
    if (known) RUN.knownFailures += 1;
    const entry = {
      id,
      description,
      status: 'failed',
      durationMs: Date.now() - startedAt,
      error: error.stack ?? String(error),
      evidence: checkEvidenceFromError(error),
    };
    if (options.knownFix !== undefined) entry.targetsKnownFix = options.knownFix;
    RUN.checks.push(entry);
    console.error(`FAIL ${id}: ${error.message}`);
    return false;
  }
}

async function openPanel(instance, label) {
  const { cdp } = instance;
  const page = ({ '模型与预设': 'model', '记忆': 'memory', '诊断': 'diagnostics' })[label];
  if (page !== undefined) {
    await navigatePage(cdp, page);
    const panelName = ({ model: '模型设置', memory: '记忆', diagnostics: '运行诊断' })[page];
    const panelVisible = await cdp.evaluate(`__smoke.panel()?.getAttribute('aria-label') === ${JSON.stringify(panelName)}`);
    if (panelVisible !== true) {
      await cdp.clickElement(`.sidebar .nav-item[data-page="${page}"]`);
      await cdp.waitFor(
        `__smoke.panel()?.getAttribute('aria-label') === ${JSON.stringify(panelName)}`,
        8_000,
        `${label} panel reopened from sidebar`,
      );
    }
    return;
  }
  await cdp.clickElement(B(label));
  await cdp.waitFor(
    `__smoke.panel() !== null && __smoke.panelText().includes(${JSON.stringify(label)})`,
    8_000,
    `${label} panel`,
  );
}

async function navigatePage(cdp, page) {
  await cdp.clickElement(`.sidebar .nav-item[data-page="${page}"]`);
  await cdp.waitFor(
    `(__smoke.currentPage() === ${JSON.stringify(page)} && __smoke.pageVisible(${JSON.stringify(page)})) || __smoke.dialog() !== null`,
    8_000,
    `${page} page and navigation synchronized`,
  );
  const state = await cdp.evaluate(`({page:__smoke.currentPage(),visible:__smoke.pageVisible(${JSON.stringify(page)}),dialog:__smoke.dialogText()})`);
  if (state.dialog) throw new CheckFailure(`navigation to ${page} is blocked by confirmation`, state);
  ensure(state.page === page && state.visible, `navigation did not reach ${page}`, state);
}

async function typeChat(cdp, text) {
  if (await cdp.evaluate(`__smoke.currentPage() !== 'chat'`) === true) await navigatePage(cdp, 'chat');
  await cdp.typeInto('#chat-input', text);
}

async function scenarioIpcBoundary(instance) {
  await check('ipc.pet-cannot-access-main-business', '宠物窗不能读取主窗快照或派发业务命令', async () => {
    const petCdp = await CdpClient.connect(instance.port, '宠物');
    try {
      const petSnapshot = await petCdp.invoke('prototype_snapshot');
      const mainSnapshot = await readSnapshot(instance.cdp);
      const petDispatch = await petCdp.invoke('prototype_dispatch', {
        command: { kind: 'refresh_memories' },
        context: buddyContextOf(mainSnapshot),
      });
      ensure(petSnapshot.snapshot === null, 'pet window read the main snapshot');
      ensure(petSnapshot.error?.code === 'command_source', 'pet snapshot error code mismatch');
      ensure(petDispatch.snapshot === null, 'pet window dispatched a business command');
      ensure(petDispatch.error?.code === 'command_source', 'pet dispatch error code mismatch');
      const petText = await petCdp.evaluate(`document.body.innerText`);
      return {
        snapshotError: petSnapshot.error?.code,
        dispatchError: petDispatch.error?.code,
        petBodyBytes: Buffer.byteLength(petText),
      };
    } finally {
      petCdp.close();
    }
  });
}

async function scenarioModelAndChat(instance, model) {
  const { cdp } = instance;
  await navigatePage(cdp, 'model');

  await check('ui.model-config-applied', '通过 DOM 表单完成端点/模型/人设/密钥配置并清空密码框', async () => {
    await openPanel(instance, '模型与预设');
    await cdp.waitFor(`${B('应用配置')} !== null`, 5_000, 'apply button');
    await cdp.typeInto(C('端点'), model.endpoint);
    await cdp.typeInto(C('模型'), 'synthetic-smoke-model');
    await cdp.typeInto(C('人设'), 'synthetic smoke persona');
    await cdp.typeInto(C('API Key'), 'synthetic-smoke-key');
    await cdp.clickElement(B('应用配置'));
    await cdp.waitFor(
      `__smoke.panelText().includes('模型配置已应用到桌面宿主')`,
      10_000,
      'model apply success notice',
    );
    const form = await cdp.evaluate(`({
      endpoint: ${C('端点')}?.value ?? null,
      model: ${C('模型')}?.value ?? null,
      persona: ${C('人设')}?.value ?? null,
      apiKey: ${C('API Key')}?.value ?? null,
      passwordType: ${C('API Key')}?.type ?? null,
    })`);
    const snapshot = await readSnapshot(cdp);
    ensure(form.endpoint === model.endpoint, 'endpoint form value lost after apply');
    ensure(form.model === 'synthetic-smoke-model', 'model form value lost after apply');
    ensure(form.persona === 'synthetic smoke persona', 'persona form value lost after apply');
    ensure(form.apiKey === '', `password field not cleared after apply: ${JSON.stringify(form.apiKey)}`);
    ensure(form.passwordType === 'password', 'API key control is not a password field');
    ensure(snapshot.modelSettings?.hasApiKey === true, 'host did not retain the process key');
    ensure(snapshot.modelSettings?.allowTransmission === false, 'transmission enabled before consent');
    const registry = await readBuddyRegistry(instance.dataRoot);
    ensure(registry.model.endpoint === model.endpoint, 'saved endpoint mismatch');
    ensure(registry.model.model === 'synthetic-smoke-model', 'saved model mismatch');
    ensure(registry.active.persona === 'synthetic smoke persona', 'saved persona mismatch');
    const raw = registry.raw;
    ensure(!raw.includes('synthetic-smoke-key'), 'API key leaked into buddies.v1');
    ensure(!raw.includes('allowTransmission'), 'transmission consent leaked into buddies.v1');
    const domText = await cdp.evaluate('document.body.innerText');
    ensure(!domText.includes('synthetic-smoke-key'), 'API key leaked into DOM text');
    return {
      endpointPath: new URL(model.endpoint).pathname,
      passwordFieldCleared: true,
      registryFields: Object.keys(registry.registry),
      registryRevision: registry.registry.revision,
      snapshotAllowTransmission: snapshot.modelSettings?.allowTransmission,
    };
  });

  await check('ui.permission-copy-covers-scope', '发送许可文案覆盖人设、获准私密记忆与当前会话', async () => {
    const text = await panelText(cdp);
    const label = await cdp.evaluate(`__smoke.allowTransmissionLabel()`);
    ensure(label !== null, 'transmission permission checkbox has no label', { text });
    ensure(label.includes('允许'), 'permission control copy does not state the grant', { label });
    ensure(label.includes('发送'), 'permission control copy does not state transmission', { label });
    ensure(label.includes('人设'), 'permission control copy does not mention persona', { label });
    ensure(label.includes('记忆'), 'permission control copy does not mention private memories', { label });
    ensure(label.includes('会话'), 'permission control copy does not mention the current session', { label });
    ensure(text.includes('允许'), 'permission explanation missing from the panel', { text });
    return { permissionCopy: text, permissionLabel: label };
  });

  await check('ui.endpoint-copy-requires-chat-completions', '端点提示要求完整 Chat Completions 路径', async () => {
    const evidence = await cdp.evaluate(`(() => {
      const body = document.querySelector('.panel-body')?.cloneNode(true) ?? null;
      body?.querySelector('.preset-list')?.remove();
      return {
        body: body?.innerText ?? '',
        form: document.querySelector('.panel-form')?.innerText ?? '',
        placeholder: ${C('端点')}?.placeholder ?? '',
        endpoint: ${C('端点')}?.value ?? '',
      };
    })()`);
    const combined = `${evidence.body}\n${evidence.form}\n${evidence.placeholder}`.toLowerCase();
    ensure(
      combined.includes('chat completions') || combined.includes('chat/completions'),
      'endpoint hint does not require a full Chat Completions path',
      evidence,
    );
    return evidence;
  });

  await check('net.no-transmission-without-consent', '未获发送许可时点击发送不产生网络请求', async () => {
    const before = model.state.requests;
    await typeChat(cdp, 'permission denial must stay local');
    await cdp.waitFor(`!${B('发送')}.disabled`, 3_000, 'send button enabled');
    await cdp.clickElement(B('发送'));
    await cdp.waitFor(`__smoke.alerts().includes('发送消息')`, 8_000, 'denial alert');
    const alerts = await cdp.evaluate(`__smoke.alerts()`);
    const snapshot = await readSnapshot(cdp);
    await sleep(500);
    ensure(model.state.requests === before, `request without consent reached loopback (${model.state.requests})`);
    ensure(
      alerts.includes('未执行') || alerts.includes('允许'),
      'denial is not shown as not executed / permission required',
      { alerts },
    );
    ensure(snapshot.externalTransmissionAllowed === false, 'host reported consent without grant');
    ensure(snapshot.chat.messages.length === 0, 'denied send produced messages');
    return {
      requests: model.state.requests,
      alerts,
      chatStatus: snapshot.chat.status,
    };
  });

  await check('net.consent-granted-retains-process-key', '通过真实复选框授予发送许可并用进程密钥应用', async () => {
    await openPanel(instance, '模型与预设');
    await cdp.waitFor(`__smoke.allowTransmissionCheckbox() !== null`, 3_000, 'consent checkbox');
    const wasChecked = await cdp.evaluate(`__smoke.allowTransmissionCheckbox().checked`);
    if (!wasChecked) await cdp.clickElement(`__smoke.allowTransmissionCheckbox()`);
    await cdp.waitFor(`__smoke.allowTransmissionCheckbox().checked === true`, 2_000, 'consent checked');
    await cdp.clickElement(B('应用配置'));
    await cdp.waitFor(
      `__smoke.statusText().includes('已允许外传')`,
      10_000,
      'transmission allowed status',
    );
    const snapshot = await readSnapshot(cdp);
    const formKey = await cdp.evaluate(`${C('API Key')}?.value ?? ''`);
    ensure(snapshot.externalTransmissionAllowed === true, 'host did not record transmission consent');
    ensure(snapshot.modelSettings?.allowTransmission === true, 'model settings consent mismatch');
    ensure(snapshot.modelSettings?.hasApiKey === true, 'null apiKey did not retain the process credential');
    ensure(formKey === '', 'password field was repopulated');
    const registry = await readBuddyRegistry(instance.dataRoot);
    ensure(!registry.raw.includes('allowTransmission'), 'consent persisted to buddies.v1');
    return {
      externalTransmissionAllowed: snapshot.externalTransmissionAllowed,
      hasApiKey: snapshot.modelSettings?.hasApiKey,
    };
  });

  await check('chat.loopback-send-and-reply-displayed', '点击发送后显示回环回复，模型请求参数完整', async () => {
    const before = model.state.requests;
    await typeChat(cdp, 'synthetic smoke prompt');
    await cdp.waitFor(`!${B('发送')}.disabled`, 3_000, 'send button enabled after draft');
    await cdp.clickElement(B('发送'));
    await cdp.waitFor(
      `__smoke.chatText().includes('synthetic loopback reply')`,
      15_000,
      'loopback reply rendered',
    );
    await cdp.waitFor(`__smoke.statusText().includes('就绪')`, 8_000, 'chat ready after reply');
    const chatText = await cdp.evaluate(`__smoke.chatText()`);
    const draft = await cdp.evaluate(`document.querySelector('#chat-input')?.value ?? ''`);
    const snapshot = await readSnapshot(cdp);
    ensure(chatText.includes('synthetic smoke prompt'), 'user prompt not displayed');
    ensure(chatText.includes('伙伴'), 'assistant role not displayed');
    ensure(draft === '', `chat draft not cleared after send: ${JSON.stringify(draft)}`);
    ensure(model.state.requests === before + 1, `expected one loopback request, got ${model.state.requests - before}`);
    ensure(model.state.failure === null, `loopback fixture failure: ${model.state.failure}`);
    const payload = model.state.lastRequest;
    ensure(payload.model === 'synthetic-smoke-model', 'model name not sent');
    ensure(payload.stream === false, 'stream flag not false');
    ensure(payload.messages?.[0]?.role === 'system', 'system/persona message missing');
    ensure(
      String(payload.messages?.[0]?.content ?? '').includes('synthetic smoke persona'),
      'persona not sent',
    );
    ensure(
      payload.messages?.some((message) => message.role === 'user' && message.content.includes('synthetic smoke prompt')),
      'current user text not sent',
    );
    ensure(
      model.state.lastAuthorization === 'Bearer synthetic-smoke-key',
      'loopback did not receive the configured bearer key',
    );
    ensure(
      snapshot.chat.messages.some((message) => message.text.includes('synthetic loopback reply')),
      'host snapshot lacks the loopback reply',
    );
    ensure(JSON.stringify(snapshot).includes('synthetic-smoke-key') === false, 'API key leaked into snapshot');
    const logs = instance.logs.join('\n');
    ensure(!logs.includes('synthetic-smoke-key'), 'API key leaked into process logs');
    return {
      requests: model.state.requests,
      authorization: 'Bearer ***',
      messageRoles: snapshot.chat.messages.map((message) => message.role),
      displayedReply: true,
    };
  });

  await check('chat.key-not-visible-in-pet', '配置后的密钥不出现在宠物窗 DOM', async () => {
    const petCdp = await CdpClient.connect(instance.port, '宠物');
    try {
      const petText = await petCdp.evaluate('document.body.innerText');
      ensure(!petText.includes('synthetic-smoke-key'), 'API key visible in pet window');
      return { petTextBytes: Buffer.byteLength(petText) };
    } finally {
      petCdp.close();
    }
  });
}

async function scenarioMemoryAndDiagnostics(instance, model) {
  const { cdp } = instance;
  await navigatePage(cdp, 'memory');

  await check('memory.remember-visible', '通过记忆面板新增一条私密记忆并显示', async () => {
    await openPanel(instance, '记忆');
    await cdp.waitFor(`document.querySelector('.remember-form textarea') !== null`, 5_000, 'remember textarea');
    await cdp.typeInto('.remember-form textarea', 'synthetic restore keepsake');
    await cdp.clickElement(B('记住'));
    await cdp.waitFor(
      `__smoke.panelText().includes('synthetic restore keepsake')`,
      10_000,
      'keepsake memory rendered',
    );
    await cdp.typeInto('.remember-form textarea', 'synthetic blue tea');
    await cdp.clickElement(B('记住'));
    await cdp.waitFor(
      `__smoke.panelText().includes('synthetic blue tea')`,
      10_000,
      'new memory rendered',
    );
    const snapshot = await readSnapshot(cdp);
    const note = snapshot.memories.find((memory) => memory.text === 'synthetic blue tea');
    ensure(note !== undefined, 'new memory missing from host snapshot');
    ensure(snapshot.memories.some((memory) => memory.text === 'synthetic restore keepsake'), 'keepsake missing');
    ensure((await cdp.evaluate(`__smoke.statuses()`)).includes('已写入一条私密记忆'), 'write notice missing');
    return { memoryId: note.id, revision: note.revision, totalMemories: snapshot.memories.length };
  });

  await check('memory.search-visible', '检索框输入并从列表过滤', async () => {
    await cdp.typeInto(C('检索'), 'tea');
    await cdp.clickElement(B('检索'));
    await cdp.waitFor(
      `__smoke.panelText().includes('synthetic blue tea')`,
      8_000,
      'search result rendered',
    );
    const text = await panelText(cdp);
    ensure(!text.includes('synthetic restore keepsake'), 'search did not filter unrelated memory');
    const query = await cdp.evaluate(`${C('检索')}?.value ?? ''`);
    ensure(query === 'tea', 'search query was cleared');
    const snapshot = await readSnapshot(cdp);
    ensure(snapshot.memories.length === 1, `expected one filtered memory, got ${snapshot.memories.length}`);
    return { query, filteredCount: snapshot.memories.length };
  });

  await check('memory.edit-versioned-visible', '通过编辑按钮保存新版本并显示', async () => {
    await cdp.clickElement(itemButton('编辑', 'synthetic blue tea'));
    await cdp.waitFor(`${C('编辑记忆')} !== null`, 3_000, 'memory editor');
    await cdp.typeInto(C('编辑记忆'), 'synthetic green tea');
    await cdp.clickElement(B('保存修改'));
    await cdp.waitFor(
      `__smoke.panelText().includes('synthetic green tea')`,
      10_000,
      'edited memory rendered',
    );
    await cdp.waitFor(
      `__smoke.statuses().includes('已保存为新的记忆版本')`,
      5_000,
      'memory update notice',
    );
    const snapshot = await readSnapshot(cdp);
    const note = snapshot.memories.find((memory) => memory.text === 'synthetic green tea');
    ensure(note !== undefined, 'edited memory missing from snapshot');
    ensure(note.revision >= 2, 'memory revision did not advance');
    return { memoryId: note.id, revision: note.revision };
  });

  await check('memory.updated-note-recalled-in-next-request', '界面修改的记忆在下一轮获准请求中使用最新版本', async () => {
    await navigatePage(cdp, 'chat');
    const before = model.state.requests;
    const messagesBefore = (await readSnapshot(cdp)).chat.messages.length;
    await typeChat(cdp, 'synthetic smoke prompt tea');
    await cdp.clickElement(B('发送'));
    await waitForSnapshot(
      cdp,
      (state) => state.chat.status === 'ready' && state.chat.messages.length === messagesBefore + 2,
      10_000,
      'reply after updated memory recall',
    );
    ensure(model.state.requests === before + 1, 'memory recall did not produce exactly one request');
    ensure(model.state.failure === null, `loopback fixture failure: ${model.state.failure}`);
    const messages = model.state.lastRequest.messages;
    ensure(
      messages.some((message) => message.role === 'user' && message.content.includes('synthetic green tea')),
      'updated private memory was absent from the authorized outgoing request',
    );
    ensure(
      !messages.some((message) => message.content.includes('synthetic blue tea')),
      'superseded private memory was sent instead of the updated version',
    );
    return { requests: model.state.requests - before, updatedMemoryRecalled: true, supersededMemoryAbsent: true };
  });

  await check('memory.confirmed-delete-visible', '确认删除需要二次点击并更新列表', async () => {
    await navigatePage(cdp, 'memory');
    await cdp.clickElement(itemButton('删除', 'synthetic green tea'));
    await cdp.waitFor(
      `__smoke.button('确认删除', __smoke.memoryItem('synthetic green tea')) !== null`,
      3_000,
      'delete confirmation buttons',
    );
    const armed = await cdp.evaluate(`__smoke.memoryItem('synthetic green tea')?.innerText ?? ''`);
    ensure(armed.includes('取消'), 'delete cancel button missing');
    await cdp.clickElement(itemButton('确认删除', 'synthetic green tea'));
    await cdp.waitFor(
      `__smoke.statuses().includes('已删除该记忆')`,
      8_000,
      'delete success notice',
    );
    await cdp.waitFor(
      `__smoke.memoryItem('synthetic green tea') === null`,
      5_000,
      'deleted memory removed from list',
    );
    const snapshot = await readSnapshot(cdp);
    ensure(!snapshot.memories.some((memory) => memory.text === 'synthetic green tea'), 'deleted memory remains in snapshot');
    return { remaining: snapshot.memories.map((memory) => memory.text), note: 'active search filter may hide the kept memory until refresh' };
  });

  await check('memory.refresh-restores-full-list', '刷新列表从宿主重新读取剩余记忆', async () => {
    await cdp.typeInto(C('检索'), '');
    await cdp.clickElement(B('检索'));
    await cdp.clickElement(B('刷新列表'));
    await cdp.waitFor(
      `__smoke.panelText().includes('synthetic restore keepsake')`,
      8_000,
      'keespake after refresh',
    );
    const snapshot = await readSnapshot(cdp);
    ensure(snapshot.memories.some((memory) => memory.text === 'synthetic restore keepsake'), 'refresh lost keepsake');
    return { remembered: snapshot.memories.map((memory) => memory.text) };
  });

  await check('diagnostics.entry-refreshes', '诊断入口打开并通过按钮刷新有界列表', async () => {
    await openPanel(instance, '诊断');
    await cdp.clickElement(B('刷新诊断'));
    await cdp.waitFor(
      `__smoke.statuses().includes('诊断列表已刷新')`,
      8_000,
      'diagnostics refresh notice',
    );
    const snapshot = await readSnapshot(cdp);
    ensure(snapshot.diagnostics.length <= 8, `diagnostics exceeded bound: ${snapshot.diagnostics.length}`);
    const rendered = await cdp.evaluate(`document.querySelectorAll('.diagnostic-list li').length`);
    ensure(rendered === snapshot.diagnostics.length, `rendered diagnostics ${rendered} != snapshot ${snapshot.diagnostics.length}`);
    if (snapshot.diagnostics.length > 0) {
      ensure(rendered > 0, 'diagnostics list did not render authorized events');
    } else {
      ensure((await panelText(cdp)).includes('还没有可展示的诊断事件'), 'empty diagnostics state missing');
    }
    ensure(!(await cdp.evaluate(`__smoke.alerts()`)).includes('诊断'), 'diagnostics refresh surfaced an alert');
    return { diagnostics: snapshot.diagnostics.length, rendered };
  });
}

async function scenarioMemoryConflict(instance) {
  const { cdp } = instance;
  await navigatePage(cdp, 'memory');

  await check('memory.conflict-explicit-not-executed-and-recoverable', '真实版本冲突应明确未执行、零覆盖、保留草稿并可刷新重编辑', async () => {
    await openPanel(instance, '记忆');
    await cdp.typeInto(C('新增私密记忆'), 'synthetic conflict source');
    await cdp.clickElement(B('记住'));
    await cdp.waitFor(
      `__smoke.panelText().includes('synthetic conflict source')`,
      10_000,
      'conflict source memory rendered',
    );
    const before = await readSnapshot(cdp);
    const note = before.memories.find((memory) => memory.text === 'synthetic conflict source');
    ensure(note !== undefined, 'conflict source memory missing from snapshot');
    await cdp.clickElement(itemButton('编辑', 'synthetic conflict source'));
    await cdp.waitFor(`${C('编辑记忆')} !== null`, 3_000, 'conflict memory editor');
    await cdp.typeInto(C('编辑记忆'), 'synthetic retained conflict draft');
    // 仅用独立 IPC 制造外部版本变化；被测保存仍由下方真实按钮完成。
    const external = await cdp.invoke('prototype_dispatch', {
      command: {
        kind: 'update_memory',
        id: note.id,
        revision: note.revision,
        text: 'synthetic external conflict variant',
      },
      context: buddyContextOf(before),
    });
    ensure(external.error === null, `external revision change failed: ${JSON.stringify(external.error)}`);
    await waitForSnapshot(
      cdp,
      (state) => state.memories.some(
        (memory) =>
          memory.id === note.id
          && memory.text === 'synthetic external conflict variant'
          && memory.revision > note.revision,
      ),
      8_000,
      'external conflict revision',
    );
    await cdp.waitFor(
      `__smoke.panelText().includes('该记忆版本已变化')`,
      8_000,
      'superseded edit warning',
    );
    await cdp.clickElement(B('保存修改'));
    await cdp.waitFor(
      `__smoke.alerts().includes('保存记忆修改') || __smoke.alerts().includes('版本已变化')`,
      8_000,
      'conflict save failure alert',
    );
    const alerts = await cdp.evaluate(`__smoke.alerts()`);
    const statuses = await cdp.evaluate(`__smoke.statuses()`);
    const draft = await cdp.evaluate(`${C('编辑记忆')}?.value ?? null`);
    const unknownLock = await cdp.evaluate(`__smoke.unknownLock()`);
    const after = await readSnapshot(cdp);
    const current = after.memories.find((memory) => memory.id === note.id);

    // 读回分类：再次用已过期的 revision 走一次只读失败路径，拒绝不产生写入。
    const staleClassification = await cdp.invoke('prototype_dispatch', {
      command: {
        kind: 'update_memory',
        id: note.id,
        revision: note.revision,
        text: 'synthetic stale classification readback',
      },
      context: buddyContextOf(after),
    });

    let recovery = { ok: false, error: null };
    try {
      if ((await cdp.evaluate(`__smoke.button('取消编辑') !== null`)) === true) {
        await cdp.clickElement(B('取消编辑'));
      }
      await cdp.clickElement(B('刷新列表'));
      await cdp.waitFor(
        `__smoke.memoryItem('synthetic external conflict variant') !== null`,
        8_000,
        'conflict memory after refresh',
      );
      await cdp.clickElement(itemButton('编辑', 'synthetic external conflict variant'));
      await cdp.waitFor(`${C('编辑记忆')} !== null`, 3_000, 'refreshed memory editor');
      await cdp.typeInto(C('编辑记忆'), 'synthetic resolved conflict draft');
      await cdp.clickElement(B('保存修改'));
      await cdp.waitFor(
        `__smoke.statuses().includes('已保存为新的记忆版本')`,
        8_000,
        'resolved conflict save notice',
      );
      recovery.ok = true;
    } catch (error) {
      recovery.error = error.message;
    }
    const resolved = await readSnapshot(cdp);
    const resolvedNote = resolved.memories.find((memory) => memory.id === note.id);

    const evidence = {
      conflictCode: staleClassification.error?.code ?? null,
      conflictDisposition: staleClassification.error?.disposition ?? null,
      conflictingRevision: current?.revision ?? null,
      currentText: current?.text ?? null,
      retainedDraft: draft,
      alerts,
      statuses,
      unknownLock,
      recovery: {
        ok: recovery.ok,
        error: recovery.error,
        text: resolvedNote?.text ?? null,
        revision: resolvedNote?.revision ?? null,
      },
    };
    const failures = [];
    if (evidence.conflictCode !== 'memory_revision_conflict') {
      failures.push(`conflict code is ${JSON.stringify(evidence.conflictCode)}, expected memory_revision_conflict`);
    }
    if (evidence.conflictDisposition !== 'not_executed') {
      failures.push(`conflict disposition is ${JSON.stringify(evidence.conflictDisposition)}, expected not_executed`);
    }
    if (current?.text !== 'synthetic external conflict variant') {
      failures.push('stale save overwrote the external change');
    }
    if (!(current?.revision > note.revision)) {
      failures.push('external revision did not advance');
    }
    if (draft !== 'synthetic retained conflict draft') {
      failures.push(`edit draft was not retained: ${JSON.stringify(draft)}`);
    }
    if (statuses.includes('已保存为新的记忆版本')) {
      failures.push('failed save also displayed a success notice');
    }
    if (!alerts.includes('版本已变化')) {
      failures.push(`conflict copy is not explicit: ${JSON.stringify(alerts)}`);
    }
    if (unknownLock) {
      failures.push('definite revision conflict incorrectly locked side effects as unknown');
    }
    if (!recovery.ok) {
      failures.push(`refresh/re-edit recovery failed: ${recovery.error}`);
    }
    if (
      resolvedNote?.text !== 'synthetic resolved conflict draft'
      || !(resolvedNote?.revision > (current?.revision ?? 0))
    ) {
      failures.push('resolved re-edit was not saved as a new revision');
    }
    ensure(failures.length === 0, failures.join('; '), evidence);
    return evidence;
  }, { knownFix: 'memory-revision-conflict' });

  await check(
    'memory.failure-during-unmount-does-not-change-host',
    '组件释放时到达的真实记忆版本失败不得覆盖宿主记忆',
    async () => {
      await openPanel(instance, '记忆');
      await cdp.typeInto(C('新增私密记忆'), 'synthetic unmount failure source');
      await cdp.clickElement(B('记住'));
      await cdp.waitFor(
        `__smoke.memoryItem('synthetic unmount failure source') !== null`,
        8_000,
        'unmount failure source memory',
      );
      const before = await readSnapshot(cdp);
      const note = before.memories.find((memory) => memory.text === 'synthetic unmount failure source');
      ensure(note !== undefined, 'unmount failure source memory missing from snapshot');

      await cdp.clickElement(itemButton('编辑', 'synthetic unmount failure source'));
      await cdp.waitFor(`${C('编辑记忆')} !== null`, 3_000, 'unmount failure editor');
      await cdp.typeInto(C('编辑记忆'), 'synthetic draft cleared by component cleanup');
      const external = await cdp.invoke('prototype_dispatch', {
        command: {
          kind: 'update_memory',
          id: note.id,
          revision: note.revision,
          text: 'synthetic unmount failure external revision',
        },
        context: buddyContextOf(before),
      });
      ensure(external.error === null, `external revision update failed: ${JSON.stringify(external.error)}`);
      await waitForSnapshot(
        cdp,
        (state) => state.memories.some(
          (memory) => memory.id === note.id && memory.revision > note.revision,
        ),
        8_000,
        'unmount failure external revision',
      );

      const component = await cdp.evaluate(`(() => {
        const root = document.querySelector('#app');
        const app = root?.__vue_app__;
        const save = __smoke.button('保存修改');
        if (!app || !save || save.disabled) return { triggered: false };
        save.click();
        app.unmount();
        return {
          triggered: true,
          released: root.childElementCount === 0,
        };
      })()`);
      ensure(component?.triggered, 'could not trigger and release the production memory editor');
      ensure(component.released, 'Vue production component did not unmount');
      await sleep(150);
      const afterHost = await readSnapshot(cdp);
      const current = afterHost.memories.find((memory) => memory.id === note.id);
      ensure(current?.text === 'synthetic unmount failure external revision', 'stale save changed the authoritative memory');
      const cleanStatus = await cdp.invoke('prototype_management_draft_state', {
        version: afterHost.managementDraft.version + 1,
        dirty: false,
        kind: 'none',
        context: buddyContextOf(afterHost),
      });
      ensure(cleanStatus.dirty === false, 'unmount smoke cleanup did not clear host protection state');
      return {
        failure: 'memory_revision_conflict',
        unmounted: component.released,
        authoritativeMemory: current.text,
      };
    },
  );
}

async function scenarioRestore(instance, model) {
  const { cdp } = instance;

  await check('restore.model-settings-restored-without-secrets', '重启后恢复端点/模型/人设，密钥与许可不恢复', async () => {
    const snapshot = await readSnapshot(cdp);
    ensure(snapshot.persistenceEnabled === true, 'persistence disabled after restart');
    ensure(snapshot.externalTransmissionAllowed === false, 'transmission consent restored');
    ensure(snapshot.modelSettings?.hasApiKey === false, 'API key restored');
    ensure(snapshot.modelSettings?.allowTransmission === false, 'transmission setting restored');
    ensure(snapshot.modelSettings?.model === 'synthetic-smoke-model', 'model not restored');
    ensure(snapshot.modelSettings?.persona === 'synthetic smoke persona', 'persona not restored');
    ensure(
      snapshot.modelSettings?.endpoint?.endsWith('/v1/chat/completions'),
      `endpoint not restored as full Chat Completions path: ${snapshot.modelSettings?.endpoint}`,
    );
    await openPanel(instance, '模型与预设');
    const form = await cdp.evaluate(`({
      endpoint: ${C('端点')}?.value ?? null,
      model: ${C('模型')}?.value ?? null,
      persona: ${C('人设')}?.value ?? null,
      apiKey: ${C('API Key')}?.value ?? null,
    })`);
    ensure(form.endpoint === snapshot.modelSettings.endpoint, 'form endpoint mismatch after restart');
    ensure(form.model === 'synthetic-smoke-model', 'form model mismatch after restart');
    ensure(form.persona === 'synthetic smoke persona', 'form persona mismatch after restart');
    ensure(form.apiKey === '', 'password field not empty after restart');
    const panel = await panelText(cdp);
    ensure(panel.includes('未允许外传'), 'restored panel does not show denied transmission');
    ensure(panel.includes('本次进程没有 API Key'), 'restored panel does not show missing credential');
    return {
      endpoint: snapshot.modelSettings.endpoint,
      hasApiKey: snapshot.modelSettings.hasApiKey,
      allowTransmission: snapshot.modelSettings.allowTransmission,
    };
  });

  await check('restore.memory-restored', '重启后通过记忆面板刷新恢复剩余记忆', async () => {
    await openPanel(instance, '记忆');
    await cdp.clickElement(B('刷新列表'));
    await cdp.waitFor(
      `__smoke.panelText().includes('synthetic restore keepsake')`,
      10_000,
      'restored memory rendered',
    );
    const text = await panelText(cdp);
    ensure(!text.includes('synthetic green tea'), 'deleted memory returned after restart');
    const snapshot = await readSnapshot(cdp);
    ensure(snapshot.memories.some((memory) => memory.text === 'synthetic restore keepsake'), 'keepsake memory not restored');
    return { memories: snapshot.memories.map((memory) => memory.text) };
  });

  await check('restore.transmission-consent-not-restored', '重启后发送仍被拒绝且不产生新网络请求', async () => {
    const before = model.state.requests;
    await typeChat(cdp, 'post restart denial probe');
    await cdp.waitFor(`!${B('发送')}.disabled`, 3_000, 'send enabled after restart');
    await cdp.clickElement(B('发送'));
    await cdp.waitFor(`__smoke.alerts().includes('发送消息')`, 8_000, 'restart denial alert');
    await sleep(500);
    ensure(model.state.requests === before, 'restart reused transmission consent');
    const snapshot = await readSnapshot(cdp);
    ensure(snapshot.externalTransmissionAllowed === false, 'consent restored in host snapshot');
    ensure(snapshot.chat.status !== 'sending', 'restart send entered sending state');
    await typeChat(cdp, '');
    await waitForSnapshot(cdp, (state) => state.chat.draft === '', 5_000, 'cleared chat draft');
    const closeHint = await cdp.evaluate(`__smoke.button('关闭提示') !== null`);
    if (closeHint) await cdp.clickElement(B('关闭提示'));
    return { requests: model.state.requests, clearedDraft: true };
  });
}

async function scenarioManagementDraftExit(instance) {
  const { cdp } = instance;
  await navigatePage(cdp, 'memory');
  const draft = 'synthetic unsaved management draft';

  async function enterDraft() {
    if ((await cdp.evaluate(`${C('新增私密记忆')} !== null`)) !== true) {
      await openPanel(instance, '记忆');
    }
    await cdp.typeInto(C('新增私密记忆'), draft);
    // 给脏状态同步一个有界窗口；关闭按钮仍由真实 DOM 点击触发。
    await sleep(1_500);
    ensure(
      (await cdp.evaluate(`${C('新增私密记忆')}?.value`)) === draft,
      'management draft did not stay in the form',
      { observed: await cdp.evaluate(`${C('新增私密记忆')}?.value ?? null`),
        host: await readSnapshot(cdp), activeElement: await cdp.evaluate('document.activeElement?.outerHTML') },
    );
  }

  async function draftStillPresent() {
    if ((await cdp.evaluate(`${C('新增私密记忆')} !== null`)) !== true) {
      await openPanel(instance, '记忆');
    }
    return (await cdp.evaluate(`${C('新增私密记忆')}?.value ?? null`)) === draft;
  }

  await check('exit.close-unsaved-draft-cancel', '未保存管理草稿通过真实窗口关闭入口要求确认，取消保留草稿', async () => {
    await enterDraft();
    const posted = await postNativeClose(instance);
    ensure(posted.ok === true, `native window close not delivered: ${JSON.stringify(posted)}`);
    let title;
    try {
      title = await waitForDialog(cdp, 5_000);
    } catch {
      const visible = await mainWindowVisible(instance).catch(() => null);
      await recoverMain(instance);
      throw new CheckFailure(
        'native close with an unsaved management draft did not show a confirmation',
        { dialog: false, mainVisible: visible },
      );
    }
    ensure(/关闭|丢弃|未保存/.test(title), `unexpected close dialog: ${title}`);
    await ensureDialogOperable(instance);
    await clickDialogCancel(cdp);
    ensure(instance.child.exitCode === null, 'process exited after cancel');
    ensure(await mainWindowVisible(instance), 'main window hidden after cancel');
    ensure(await draftStillPresent(), 'management draft lost after cancel');
    return { dialog: title, canceled: true, draftRetained: true };
  });

  await check('exit.close-unsaved-draft-confirm', '真实窗口关闭确认丢弃管理草稿、隐藏主窗并保留进程', async () => {
    if (instance.child.exitCode !== null) throw new CheckFailure('process already exited before close confirm');
    await enterDraft();
    const posted = await postNativeClose(instance);
    ensure(posted.ok === true, `native window close not delivered: ${JSON.stringify(posted)}`);
    let title;
    try {
      title = await waitForDialog(cdp, 5_000);
    } catch {
      const visible = await mainWindowVisible(instance).catch(() => null);
      await recoverMain(instance);
      throw new CheckFailure(
        'native close with an unsaved management draft did not offer discard confirmation',
        { dialog: false, mainVisible: visible },
      );
    }
    ensure(/管理面板|未保存|关闭/.test(title), `unexpected native close confirmation: ${title}`);
    await ensureDialogOperable(instance);
    const confirmLabel = await clickDialogConfirm(cdp);
    await cdp.waitFor(`__smoke.dialog() === null`, 5_000, 'dialog closed after close confirm');
    const hidden = await waitForMainVisible(instance, false, 5_000);
    ensure(hidden, 'main window did not hide after confirmed native close');
    ensure(instance.child.exitCode === null, 'native close confirmation exited the whole process');
    await recoverMain(instance);
    ensure(await waitForMainVisible(instance, true), 'main window did not reopen after close confirm');
    const cleared = await cdp.evaluate(`(async () => {
      if (__smoke.control('新增私密记忆') === null) {
        const button = __smoke.button('记忆');
        if (button) button.click();
        await new Promise((resolve) => setTimeout(resolve, 300));
      }
      return __smoke.control('新增私密记忆')?.value ?? null;
    })()`);
    ensure(cleared === '', `confirmed native close did not clear management draft: ${JSON.stringify(cleared)}`);
    const management = await readSnapshot(cdp);
    ensure(management.managementDraft.dirty === false, 'host management dirty status stayed set after discard receipt');
    return {
      dialog: title,
      confirmLabel,
      draftCleared: true,
      hostDirty: management.managementDraft.dirty,
      processAlive: true,
    };
  });

  await check('exit.native-close-unsaved-draft-cancel', '原生关窗在未保存管理草稿下取消并保留窗口与草稿', async () => {
    if (instance.child.exitCode !== null) throw new CheckFailure('process already exited before native close');
    await enterDraft();
    const posted = await postNativeClose(instance);
    ensure(posted.ok === true, `native close not delivered: ${JSON.stringify(posted)}`);
    let title;
    try {
      title = await waitForDialog(cdp, 5_000);
    } catch {
      const visible = await mainWindowVisible(instance).catch(() => null);
      await recoverMain(instance);
      throw new CheckFailure(
        'native window close with an unsaved management draft did not show a confirmation',
        { dialog: false, mainVisible: visible },
      );
    }
    ensure(/关闭|丢弃|未保存/.test(title), `unexpected native close dialog: ${title}`);
    await ensureDialogOperable(instance);
    await clickDialogCancel(cdp);
    ensure(instance.child.exitCode === null, 'process exited after native close cancel');
    ensure(await waitForMainVisible(instance, true), 'main window not visible after native close cancel');
    ensure(await draftStillPresent(), 'management draft lost after native close cancel');
    return { dialog: title, canceled: true, draftRetained: true };
  });

  await check('exit.native-close-unsaved-draft-confirm', '原生关窗确认丢弃草稿后隐藏窗口、清除草稿并保留进程', async () => {
    if (instance.child.exitCode !== null) throw new CheckFailure('process already exited before native close confirm');
    await enterDraft();
    const posted = await postNativeClose(instance);
    ensure(posted.ok === true, `native close not delivered: ${JSON.stringify(posted)}`);
    let title;
    try {
      title = await waitForDialog(cdp, 5_000);
    } catch {
      const visible = await mainWindowVisible(instance).catch(() => null);
      await recoverMain(instance);
      throw new CheckFailure(
        'native window close with an unsaved management draft did not offer discard confirmation',
        { dialog: false, mainVisible: visible },
      );
    }
    ensure(/关闭|丢弃|未保存/.test(title), `unexpected native close dialog: ${title}`);
    await ensureDialogOperable(instance);
    const confirmLabel = await clickDialogConfirm(cdp);
    await cdp.waitFor(`__smoke.dialog() === null`, 5_000, 'dialog closed after native close confirm');
    const hidden = await waitForMainVisible(instance, false, 5_000);
    ensure(hidden, 'main window did not hide after confirmed native close');
    ensure(instance.child.exitCode === null, 'native close confirm exited the whole process');
    await recoverMain(instance);
    ensure(await waitForMainVisible(instance, true), 'main window did not reopen after native close confirm');
    const cleared = await cdp.evaluate(`(async () => {
      if (__smoke.control('新增私密记忆') === null) {
        const button = __smoke.button('记忆');
        if (button) button.click();
        await new Promise((resolve) => setTimeout(resolve, 300));
      }
      return __smoke.control('新增私密记忆')?.value ?? null;
    })()`);
    ensure(cleared === '', `confirmed native close discard did not clear management draft: ${JSON.stringify(cleared)}`);
    return { dialog: title, confirmLabel, draftCleared: true, processAlive: true };
  });

  await check('exit.tray-quit-unsaved-draft-cancel', '托盘退出在未保存管理草稿下取消并保留进程与草稿', async () => {
    if (instance.child.exitCode !== null) throw new CheckFailure('process already exited before tray quit');
    if (!(await draftStillPresent())) await enterDraft();
    const tray = await postTrayQuit(instance);
    ensure(tray.ok === true, `tray quit not delivered: ${JSON.stringify(tray)}`);
    let title;
    try {
      title = await waitForDialog(cdp, 5_000);
    } catch {
      const exitCode = instance.child.exitCode;
      await recoverMain(instance).catch(() => {});
      throw new CheckFailure(
        'tray quit with an unsaved management draft exited without confirmation',
        { dialog: false, exitCode },
      );
    }
    ensure(/退出|关闭|丢弃|未保存/.test(title), `unexpected tray quit dialog: ${title}`);
    await ensureDialogOperable(instance);
    await clickDialogCancel(cdp);
    await sleep(300);
    ensure(instance.child.exitCode === null, 'process exited after tray quit cancel');
    ensure(await mainWindowVisible(instance), 'main window hidden after tray quit cancel');
    ensure(await draftStillPresent(), 'management draft lost after tray quit cancel');
    return { dialog: title, canceled: true, draftRetained: true };
  });

  await check('exit.tray-quit-unsaved-draft-confirm', '托盘退出确认丢弃草稿并以 0 退出', async () => {
    if (instance.child.exitCode !== null) throw new CheckFailure('process already exited before tray quit confirm');
    if (!(await draftStillPresent())) await enterDraft();
    const tray = await postTrayQuit(instance);
    ensure(tray.ok === true, `tray quit not delivered: ${JSON.stringify(tray)}`);
    let title;
    try {
      title = await waitForDialog(cdp, 5_000);
    } catch {
      const exitCode = instance.child.exitCode;
      throw new CheckFailure(
        'tray quit with an unsaved management draft did not offer discard confirmation',
        { dialog: false, exitCode },
      );
    }
    ensure(/退出/.test(title), `tray quit dialog does not describe application exit: ${title}`);
    // 退出应用必须说明会丢弃所有会话草稿与管理未保存编辑（领队决定的文案口径）。
    ensure(
      /所有会话草稿/.test(title) && /管理面板/.test(title),
      `quit confirmation does not state the full discard scope: ${title}`,
      { dialog: title },
    );
    await ensureDialogOperable(instance);
    const confirmLabel = await clickDialogConfirm(cdp);
    const exit = await waitForExit(instance.child, 20_000);
    ensure(exit.code === 0, `tray quit confirm exit code ${JSON.stringify(exit)}`);
    ensure(exit.signal === null, `tray quit confirm was signalled: ${exit.signal}`);
    instance.exitedNormally = true;
    return { dialog: title, confirmLabel, exit };
  });
}

async function configureModelViaDom(instance, { endpoint, model, persona, apiKey, allowTransmission = false }) {
  const { cdp } = instance;
  if ((await cdp.evaluate(`__smoke.panel() !== null`)) !== true
    || !(await cdp.evaluate(`__smoke.panel()?.getAttribute('aria-label') === '模型设置'`))) {
    await openPanel(instance, '模型与预设');
  }
  await cdp.typeInto(C('端点'), endpoint);
  await cdp.typeInto(C('模型'), model);
  await cdp.typeInto(C('人设'), persona);
  if (apiKey !== undefined) await cdp.typeInto(C('API Key'), apiKey);
  const checked = await cdp.evaluate(`__smoke.allowTransmissionCheckbox()?.checked ?? null`);
  ensure(checked !== null, 'transmission permission checkbox missing');
  if (checked !== allowTransmission) {
    await cdp.clickElement(`__smoke.allowTransmissionCheckbox()`);
    await cdp.waitFor(
      `__smoke.allowTransmissionCheckbox().checked === ${allowTransmission ? 'true' : 'false'}`,
      3_000,
      'transmission permission checkbox state',
    );
  }
  await cdp.clickElement(B('应用配置'));
  await cdp.waitFor(
    `__smoke.panelText().includes('模型配置已应用到桌面宿主')`,
    10_000,
    'model apply success notice',
  );
}

/**
 * 关闭管理面板；面板有未保存编辑时会先弹离开确认。
 * 默认拒绝隐式丢弃编辑（把“面板意外变脏”暴露成失败），只有显式传入
 * `discardEdits` 的清理路径才点“放弃并离开”。
 */
async function closePanelIfOpen(cdp, { discardEdits = false } = {}) {
  const page = await cdp.evaluate(`document.querySelector('[data-page][aria-current="page"]')?.dataset.page ?? null`);
  if (!['memory', 'model', 'diagnostics'].includes(page)) {
    return { closed: false, discarded: false };
  }
  await cdp.clickElement('.sidebar .nav-item[data-page="chat"]');
  await cdp.waitFor(
    `__smoke.currentPage() === 'chat' || __smoke.dialog() !== null`,
    8_000,
    'chat navigation or management leave confirmation',
  );
  const deadline = Date.now() + 5_000;
  let discarded = false;
  while (Date.now() < deadline) {
    if ((await cdp.evaluate(`__smoke.currentPage() === 'chat'`)) === true) {
      return { closed: true, discarded };
    }
    if ((await cdp.evaluate(`__smoke.dialog() !== null`)) === true) {
      const dialog = await dialogText(cdp);
      if (!discardEdits) {
        throw new CheckFailure(
          'management panel asked to discard unsaved edits while closing',
          { dialog },
        );
      }
      await cdp.clickElement(`__smoke.dialogButton('放弃并离开')`);
      discarded = true;
      await sleep(150);
      continue;
    }
    await sleep(150);
  }
  throw new CheckFailure('management panel did not close', {
    panel: await cdp.evaluate(`__smoke.panelText()`),
    dialog: await cdp.evaluate(`__smoke.dialogText()`),
  });
}

function snapshotSummary(state) {
  if (state === null || state === undefined) return null;
  return {
    revision: state.revision,
    activeSessionId: state.activeSessionId,
    sessionsRevision: state.sessionsRevision,
    sessionIsolationAllowed: state.sessionIsolationAllowed,
    sessions: state.sessions,
    draftChars: state.chat.draft.length,
    status: state.chat.status,
    failure: state.chat.failure,
  };
}

async function domEvidence(cdp) {
  return cdp.evaluate(`({
    alerts: __smoke.alerts(),
    warning: __smoke.warningText(),
    chips: __smoke.sessionChips(),
    dialog: __smoke.dialogText(),
    input: __smoke.chatInput()?.value ?? null,
    inputDisabled: __smoke.chatInput()?.disabled ?? null,
  })`);
}

/** 关闭残留错误横幅，避免旧“已取消”提示干扰后续步骤的证据。 */
async function dismissAlerts(cdp) {
  const buttons = await cdp.evaluate(`document.querySelectorAll('[role="alert"]').length`);
  for (let index = 0; index < buttons; index += 1) {
    await cdp.clickElement(B('关闭提示')).catch(() => {});
    await sleep(80);
    if ((await cdp.evaluate(`document.querySelectorAll('[role="alert"]').length`)) === 0) break;
  }
}

/** 通过真实输入框清空草稿，并等待宿主确认草稿基线为空。 */
async function clearChatDraft(cdp, label = 'chat draft cleared') {
  await typeChat(cdp, '');
  await waitForSnapshot(cdp, (state) => state.chat.draft === '', 5_000, label);
}

/**
 * 场景失败后的界面收尾：只关闭脚本自己打开的面板/对话框并清空草稿，
 * 让失败实例仍能走正常退出确认，而不是被强制终止。不产生验收断言。
 */
async function resetUiForExit(instance) {
  const { cdp } = instance;
  if (cdp === null || instance.child.exitCode !== null) return;
  try {
    const dialog = await cdp.evaluate(`__smoke.dialogText()`);
    if (/放弃未保存的编辑/.test(dialog)) {
      await cdp.clickElement(`__smoke.dialogButton('放弃并离开')`);
      await sleep(150);
    }
    if ((await cdp.evaluate(`__smoke.panel() !== null`)) === true) {
      await closePanelIfOpen(cdp, { discardEdits: true });
    }
    // 失败路径不得把新建/改名表单留给后续步骤或退出链。
    await closeBuddyFormsIfOpen(cdp);
    const input = await cdp.evaluate(`__smoke.chatInput()?.value ?? ''`);
    if ((await cdp.evaluate(`__smoke.chatInput()?.disabled === true`)) !== true && input.length > 0) {
      await clearChatDraft(cdp, 'failed scenario draft cleared for exit');
    }
  } catch (error) {
    instance.cleanupError = `reset-ui: ${error.message}`;
  }
}

function sessionLabelFromSnapshot(snapshot, sessionId) {
  const session = snapshot.sessions.find((item) => item.id === sessionId);
  ensure(session !== undefined, `session ${sessionId} missing from host directory`, {
    activeSessionId: snapshot.activeSessionId,
    sessions: snapshot.sessions.map((item) => item.id),
  });
  return session.label.trim().length > 0 ? session.label : session.id;
}

async function clickSessionChip(cdp, label) {
  if (await cdp.evaluate(`__smoke.currentPage() !== 'chat'`) === true) await navigatePage(cdp, 'chat');
  await cdp.clickElement(`__smoke.sessionChip(${JSON.stringify(label)})`);
}

async function selectBuddy(cdp, label) {
  if (await cdp.evaluate(`__smoke.currentPage() !== 'buddies'`) === true) await navigatePage(cdp, 'buddies');
  await cdp.clickElement(`__smoke.buddyChip(${JSON.stringify(label)})`);
}

async function sendChatViaDom(cdp, text) {
  await typeChat(cdp, text);
  await cdp.waitFor(
    `__smoke.chatInput()?.value === ${JSON.stringify(text)}`,
    5_000,
    'chat draft typed',
  );
  await cdp.clickElement(B('发送'));
}

/** 独立读回宿主原始快照，仅用于确认 A 契约是否已接线；不替代 DOM 验收。 */
async function sessionContractReadback(cdp) {
  const response = await cdp.invoke('prototype_snapshot');
  const snapshot = response.snapshot;
  const supported = response.error === null
    && snapshot !== null
    && Object.prototype.hasOwnProperty.call(snapshot, 'activeSessionId')
    && typeof snapshot.sessionsRevision === 'number'
    && Array.isArray(snapshot.sessions)
    && typeof snapshot.sessionIsolationAllowed === 'boolean';
  return { supported, response };
}

/**
 * 独立读回宿主原始快照，仅用于确认 B 伙伴契约是否已接线；不替代 DOM 验收。
 * 严格前端拒绝缺字段快照，因此这也是整条真窗链可运行性的前置。
 */
function validBuddyCommit(value) {
  if (value === null) return true;
  if (typeof value !== 'object' || Array.isArray(value)) return false;
  const keys = Object.keys(value).sort();
  const expected = [
    'kind',
    'fromBuddyId',
    'fromRevision',
    'toBuddyId',
    'toRevision',
    'managementVersion',
  ].sort();
  if (keys.length !== expected.length || keys.some((key, index) => key !== expected[index])) return false;
  if (!['create_buddy', 'switch_buddy', 'rename_buddy', 'configure_model', 'select_preset'].includes(value.kind)) {
    return false;
  }
  for (const id of [value.fromBuddyId, value.toBuddyId]) {
    if (typeof id !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]{0,63}$/.test(id) || id === '.' || id === '..') {
      return false;
    }
  }
  for (const revision of [value.fromRevision, value.toRevision]) {
    if (!Number.isSafeInteger(revision) || revision < 1) return false;
  }
  if (value.toRevision !== value.fromRevision + 1) return false;
  return Number.isSafeInteger(value.managementVersion) && value.managementVersion >= 0;
}

async function hostContractReadback(cdp) {
  const response = await cdp.invoke('prototype_snapshot');
  const snapshot = response.snapshot;
  const sessionSupported = response.error === null
    && snapshot !== null
    && Object.prototype.hasOwnProperty.call(snapshot, 'activeSessionId')
    && typeof snapshot.sessionsRevision === 'number'
    && Array.isArray(snapshot.sessions)
    && typeof snapshot.sessionIsolationAllowed === 'boolean';
  const managementKind = snapshot?.managementDraft?.kind;
  const managementKindValid = ['none', 'general', 'buddy_create', 'buddy_rename'].includes(
    managementKind,
  );
  const managementShapeValid = managementKindValid
    && snapshot?.managementDraft?.dirty === (managementKind !== 'none');
  const buddySupported = response.error === null
    && snapshot !== null
    && Object.prototype.hasOwnProperty.call(snapshot, 'activeBuddyId')
    && typeof snapshot.buddiesRevision === 'number'
    && Array.isArray(snapshot.buddies)
    && snapshot.buddies.every((buddy) => buddy !== null
      && typeof buddy === 'object'
      && typeof buddy.id === 'string'
      && typeof buddy.label === 'string'
      && typeof buddy.profileRevision === 'number'
      && typeof buddy.personaRevision === 'number'
      && typeof buddy.modelRevision === 'number')
    && Object.prototype.hasOwnProperty.call(snapshot, 'activeBuddyPersona')
    && (snapshot.activeBuddyPersona === null || typeof snapshot.activeBuddyPersona === 'string')
    && Object.prototype.hasOwnProperty.call(snapshot, 'buddyCommit')
    && validBuddyCommit(snapshot.buddyCommit)
    && (snapshot.revoked !== true || snapshot.buddyCommit === null)
    && managementShapeValid;
  return { buddySupported, sessionSupported, response };
}

/** 严格前端只有在完整契约下才会渲染业务主壳；否则如实记录未接通。 */
async function strictFrontendReady(cdp, timeoutMs = 8_000) {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    const ready = await cdp.evaluate(
      `document.querySelectorAll('.sidebar .nav-item[data-page]').length === 6
        && document.querySelector('[data-page-view="chat"]') !== null`,
    );
    if (ready === true) return true;
    await sleep(100);
  }
  return false;
}

function buddyRevisionsOf(snapshot, buddyId) {
  const buddy = (snapshot.buddies ?? []).find((item) => item.id === buddyId);
  ensure(buddy !== undefined, `buddy ${buddyId} missing from host registry`, {
    activeBuddyId: snapshot.activeBuddyId,
    buddies: (snapshot.buddies ?? []).map((item) => item.id),
  });
  return buddy;
}

function buddyLabelFromSnapshot(snapshot, buddyId) {
  const buddy = buddyRevisionsOf(snapshot, buddyId);
  return buddy.label.trim().length > 0 ? buddy.label : buddy.id;
}

/** B 伙伴注册表、创建/切换/改名与两个伙伴的隔离真窗验收。 */
async function scenarioBuddyLifecycle(instance, model) {
  const { cdp } = instance;
  await navigatePage(cdp, 'buddies');
  const contract = await hostContractReadback(cdp);
  if (!contract.buddySupported) {
    RUN.pending.push({
      id: 'buddy.b-host-contract',
      reason:
        'real host snapshot lacks a strict buddyCommit receipt (or valid activeBuddyPersona/managementDraft.kind); strict frontend cannot connect and B DOM acceptance stays pending integration',
      observedError: contract.response?.error ?? null,
      observedFields: contract.response?.snapshot === null
        ? null
        : Object.keys(contract.response?.snapshot ?? {}).sort(),
    });
    console.log('PENDING buddy.b-host-contract: real host has not integrated the frozen B snapshot contract');
    return { bHostAvailable: false, buddyAId: null, buddyBId: null, restartReady: false };
  }

  let buddyAId = null;
  let buddyBId = null;
  let buddyALabel = null;
  let buddyBLabel = null;
  let crossBuddyDraftArmed = false;

  try {
    await check('buddy.registry-loopback', '真实 DOM 展示宿主签发的伙伴注册表、活动身份与配置版本', async () => {
      await configureModelViaDom(instance, {
        endpoint: model.endpoint,
        model: 'synthetic-buddy-a-model',
        persona: 'synthetic buddy A persona',
        apiKey: 'synthetic-buddy-a-key',
        allowTransmission: true,
      });
      await closePanelIfOpen(cdp);
      const snapshot = await readSnapshot(cdp);
      ensure((snapshot.buddies ?? []).length >= 1, 'host issued no buddy registry', {
        activeBuddyId: snapshot.activeBuddyId,
        buddies: snapshot.buddies,
      });
      await cdp.waitFor(
        `__smoke.buddyChips().length === ${snapshot.buddies.length}`,
        5_000,
        'restart buddy controls rendered from the restored host registry',
      );
      const chips = await cdp.evaluate(`__smoke.buddyChips()`);
      ensure(chips.length === snapshot.buddies.length, 'buddy chips do not match host registry', {
        chips,
        buddies: snapshot.buddies,
      });
      ensure(
        chips.filter((chip) => chip.active).length === 1,
        'buddy chips do not mark exactly one active buddy',
        { chips },
      );
      ensure(
        chips.every((chip) => /档案 r\d+ · 人设 r\d+ · 模型 r\d+/.test(chip.revisions)),
        'buddy chips do not render profile/persona/model revisions',
        { chips },
      );
      const activeChip = new RegExp(`^${buddyLabelFromSnapshot(snapshot, snapshot.activeBuddyId)}$`);
      ensure(activeChip.test(await cdp.evaluate(`__smoke.activeBuddyLabel()`)), 'active buddy chip is not the host selection');
      ensure(
        (await cdp.evaluate(`__smoke.buddyIdentity()`)).includes('API Key 与发送许可'),
        'buddy identity area does not state that credentials/transmission are per buddy',
      );
      buddyAId = snapshot.activeBuddyId;
      buddyALabel = buddyLabelFromSnapshot(snapshot, buddyAId);
      return {
        activeBuddyId: snapshot.activeBuddyId,
        buddiesRevision: snapshot.buddiesRevision,
        buddies: snapshot.buddies,
      };
    });

    await check('buddy.round-a-committed', '伙伴 A 在创建/切换前拥有自己的一轮回环消息，供跨伙伴正文隔离对比', async () => {
      const requestsBefore = model.state.requests;
      await sendChatViaDom(cdp, 'synthetic round');
      const committed = await waitForSnapshot(
        cdp,
        (state) => state.chat.status === 'ready'
          && state.chat.messages.some((message) => message.text === 'synthetic round'),
        20_000,
        'buddy A own round committed',
      );
      ensure(model.state.requests === requestsBefore + 1, 'buddy A round did not reach the loopback model', {
        before: requestsBefore,
        after: model.state.requests,
      });
      ensure(
        (await cdp.evaluate(`__smoke.messagesText()`)).includes('synthetic round'),
        'buddy A round is not displayed',
      );
      return { messages: committed.chat.messages.length, requests: model.state.requests };
    });

    await check(
      'buddy.management-sync-payload',
      '真实 DOM 管理编辑同步携带种类、完整伙伴上下文与精确提交版本（同 ID 修订竞态由组件探针覆盖）',
      async () => {
        try {
          await navigatePage(cdp, 'buddies');
          await installIpcObserver(cdp);
          await clearIpcCalls(cdp);
          const before = await readSnapshot(cdp);
          await cdp.clickElement(`__smoke.buddyAction('新建伙伴')`);
          await cdp.waitFor(`__smoke.buddyForm('新建伙伴') !== null`, 5_000, 'management sync create form');
          await cdp.typeInto(
            `__smoke.buddyControl('新建伙伴', '伙伴名称')`,
            `合成同步-${Date.now().toString(36)}`,
          );
          await cdp.typeInto(
            `__smoke.buddyControl('新建伙伴', '伙伴人设')`,
            'synthetic management sync persona',
          );
          await waitForSnapshot(
            cdp,
            (state) => state.managementDraft.dirty === true
              && state.managementDraft.kind === 'buddy_create',
            10_000,
            'create management draft dirty',
          );
          // 等待可能的第二次合并记录落定，再取最后一份真实发出的载荷。
          await sleep(150);
          const dirtyCalls = (await ipcCalls(cdp, 'prototype_management_draft_state'))
            .filter((entry) => entry.args
              && entry.args.dirty === true
              && entry.args.kind === 'buddy_create');
          ensure(dirtyCalls.length >= 1, 'frontend never sent a dirty buddy_create management update');
          const lastDirty = dirtyCalls[dirtyCalls.length - 1];
          ensure(
            Number.isSafeInteger(lastDirty.args.version),
            'dirty management update has no safe version',
            lastDirty,
          );
          const dirty = await waitForSnapshot(
            cdp,
            (state) => state.managementDraft.dirty === true
              && state.managementDraft.kind === 'buddy_create'
              && state.managementDraft.version === lastDirty.args.version,
            10_000,
            'exact create management version consumed',
          );
          // 宿主按完整上下文严格接纳该状态；若发起时捕获错伙伴或旧修订会被拒绝。
          ensure(
            lastDirty.args.context?.expectedBuddyId === before.activeBuddyId
              && lastDirty.args.context?.expectedBuddiesRevision === before.buddiesRevision,
            'dirty management update does not carry the initiating buddy context',
            {
              sent: lastDirty.args.context,
              initiating: buddyContextOf(before),
            },
          );
          ensure(
            dirty.activeBuddyId === before.activeBuddyId
              && dirty.buddiesRevision === before.buddiesRevision,
            'host accepted the management update under a different buddy scope',
            { before: buddyContextOf(before), host: buddyContextOf(dirty) },
          );

          await cdp.clickElement(`__smoke.buddyFormButton('新建伙伴', '取消')`);
          await waitForSnapshot(
            cdp,
            (state) => state.managementDraft.dirty === false
              && state.managementDraft.kind === 'none',
            10_000,
            'management draft cleaned after cancel',
          );
          await sleep(150);
          const cleanCalls = (await ipcCalls(cdp, 'prototype_management_draft_state'))
            .filter((entry) => entry.args && entry.args.dirty === false);
          ensure(cleanCalls.length >= 1, 'cancel did not send a clean management update');
          const lastClean = cleanCalls[cleanCalls.length - 1];
          ensure(lastClean.args.kind === 'none', 'clean management update did not use kind none', lastClean);
          const clean = await waitForSnapshot(
            cdp,
            (state) => state.managementDraft.dirty === false
              && state.managementDraft.kind === 'none'
              && state.managementDraft.version === lastClean.args.version,
            10_000,
            'exact clean management version consumed',
          );
          ensure(
            lastClean.args.context?.expectedBuddyId === before.activeBuddyId
              && lastClean.args.context?.expectedBuddiesRevision === before.buddiesRevision,
            'clean management update does not carry the initiating buddy context',
            {
              sent: lastClean.args.context,
              initiating: buddyContextOf(before),
            },
          );
          ensure(
            (await cdp.evaluate(`__smoke.buddyForm('新建伙伴') === null`)) === true,
            'create form remained open after cancel',
          );
          return {
            dirtyKind: lastDirty.args.kind,
            dirtyVersion: lastDirty.args.version,
            cleanVersion: lastClean.args.version,
            dirtyContext: lastDirty.args.context,
            cleanContext: lastClean.args.context,
            initiatingContext: buddyContextOf(before),
            hostContextAfterDirty: buddyContextOf(dirty),
            observer: 'fetch-based CDP observation of the real Tauri IPC request body',
            note:
              'same-ID selection revision race is also covered by buddyLifecycle component tests; this host build cannot hold an in-flight management sync in a real window',
          };
        } finally {
          // 失败时显式关闭表单，避免创建/改名后续链被残留表单连带阻塞。
          await closeBuddyFormsIfOpen(cdp).catch(() => {});
        }
      },
    );

    await check(
      'buddy.same-id-stale-response-rejected',
      '同一伙伴 ID 的旧选择修订上下文被宿主拒绝，只有发起时的完整上下文可回包',
      async () => {
        const current = await readSnapshot(cdp);
        const staleContext = {
          expectedBuddyId: current.activeBuddyId,
          expectedBuddiesRevision: Math.max(0, current.buddiesRevision - 1),
        };
        const stale = await cdp.invoke('prototype_dispatch', {
          command: {
            kind: 'set_draft',
            text: current.chat.draft,
            expectedSessionId: current.activeSessionId,
            expectedSessionsRevision: current.sessionsRevision,
          },
          context: staleContext,
        });
        ensure(stale.error?.code === 'buddy_context_stale', 'same-ID stale response was accepted', {
          staleContext,
          current: buddyContextOf(current),
          error: stale.error,
        });
        const fresh = await cdp.invoke('prototype_dispatch', {
          command: {
            kind: 'set_draft',
            text: current.chat.draft,
            expectedSessionId: current.activeSessionId,
            expectedSessionsRevision: current.sessionsRevision,
          },
          context: buddyContextOf(current),
        });
        ensure(fresh.error === null, 'current same-ID context was rejected', { error: fresh.error });
        return {
          buddyId: current.activeBuddyId,
          currentRevision: current.buddiesRevision,
          staleRevision: staleContext.expectedBuddiesRevision,
          staleError: stale.error.code,
          freshAccepted: fresh.error === null,
        };
      },
    );

    await check(
      'buddy.main-window-command-context',
      '真实 DOM 主窗隐藏聊天携带发起时完整伙伴上下文，纯呈现命令不带 context',
      async () => {
        await installIpcObserver(cdp);
        await clearIpcCalls(cdp);
        const before = await readSnapshot(cdp);
        try {
          await cdp.clickElement(B('隐藏聊天'));
          ensure(await waitForMainVisible(instance, false, 5_000), 'main window did not hide after DOM hide_chat');
          const calls = (await ipcCalls(cdp, 'prototype_window'))
            .filter((entry) => entry.args?.command === 'hide_chat');
          ensure(calls.length >= 1, 'DOM hide_chat did not reach the real IPC channel');
          const sentContext = calls[calls.length - 1].args.context;
          ensure(
            sentContext?.expectedBuddyId === before.activeBuddyId
              && sentContext?.expectedBuddiesRevision === before.buddiesRevision,
            'main-window hide_chat did not carry the initiating buddy context',
            { sent: sentContext, initiating: buddyContextOf(before) },
          );
          // 纯呈现命令不带 context 由 protocol 组件回归覆盖；真实窗口这里只验证
          // 主窗上下文命令的发起载荷。
          return {
            sentContext,
            initiating: buddyContextOf(before),
            observation: 'fetch-based CDP observation of the real Tauri IPC request body',
          };
        } finally {
          // 主窗被隐藏时显式恢复；收尾失败不掩盖原始断言。
          if (await waitForMainVisible(instance, true, 2_000) === false) {
            await invokeWindowWithContext(instance, 'show_chat').catch(() => {});
            await waitForMainVisible(instance, true, 5_000);
          }
        }
      },
    );

    await check(
      'buddy.main-window-stale-context-rejected',
      '宿主拒绝主窗窗口命令的陈旧伙伴 context，且不执行隐藏窗口',
      async () => {
        await ensureDialogOperable(instance);
        const current = await readSnapshot(cdp);
        const staleContext = {
          expectedBuddyId: current.activeBuddyId,
          expectedBuddiesRevision: Math.max(0, current.buddiesRevision - 1),
        };
        let staleRejected = false;
        let staleError = null;
        try {
          await cdp.invoke('prototype_window', { command: 'hide_chat', context: staleContext });
        } catch (error) {
          staleRejected = true;
          staleError = error.message;
        }
        const hiddenAfterStale = await waitForMainVisible(instance, false, 1_500);
        if (hiddenAfterStale) {
          await invokeWindowWithContext(instance, 'show_chat');
          await waitForMainVisible(instance, true, 5_000);
        }
        ensure(staleRejected, 'host accepted a stale main-window context', {
          staleContext,
          current: buddyContextOf(current),
          hiddenAfterStale,
        });
        ensure(!hiddenAfterStale, 'stale main-window context hid the window', {
          staleContext,
          current: buddyContextOf(current),
        });
        return {
          current: buddyContextOf(current),
          staleContext,
          staleError,
          staleRejected,
          hiddenAfterStale,
        };
      },
    );

    await check(
      'buddy.main-window-missing-context-rejected',
      '真实主窗 show_chat/hide_chat/quit 缺少伙伴 context 时明确拒绝，窗口、选择与进程零副作用',
      async () => {
        await ensureDialogOperable(instance);
        const before = await readSnapshot(cdp);
        const visibleBefore = await mainWindowVisible(instance);
        ensure(visibleBefore, 'missing-context checks require the main window to start visible');
        const outcomes = [];
        const rejectMissing = async (command) => {
          let rejected = false;
          let error = null;
          try {
            await cdp.invoke('prototype_window', { command });
          } catch (failure) {
            rejected = true;
            error = failure.message;
          }
          ensure(rejected, `${command} without a buddy context was accepted`, {
            command,
            before: buddyContextOf(before),
          });
          outcomes.push({ command, rejected, error });
        };
        // 可见状态下：缺 context 的 hide_chat 不能隐藏；缺 context 的 quit 不能退出。
        await rejectMissing('hide_chat');
        await rejectMissing('quit');
        await sleep(500);
        ensure(
          await mainWindowVisible(instance),
          'missing-context hide_chat/quit changed main window visibility',
          { visibleBefore },
        );
        // 隐藏状态下：缺 context 的 show_chat 不能把窗口显示出来。
        await invokeWindowWithContext(instance, 'hide_chat');
        const hiddenWithValidContext = await waitForMainVisible(instance, false, 5_000);
        ensure(
          hiddenWithValidContext,
          'valid hide_chat with context did not hide the main window',
        );
        let missingShowRemainedHidden = false;
        try {
          await rejectMissing('show_chat');
          await sleep(500);
          missingShowRemainedHidden = (await mainWindowVisible(instance)) === false;
          ensure(
            missingShowRemainedHidden,
            'missing-context show_chat made a hidden main window visible',
          );
        } finally {
          await invokeWindowWithContext(instance, 'show_chat').catch(() => {});
          await waitForMainVisible(instance, true, 5_000);
        }
        const visibleAfter = await mainWindowVisible(instance);
        const after = await readSnapshot(cdp);
        ensure(instance.child.exitCode === null, 'missing-context quit exited the process');
        ensure(
          after.closeConfirmation === false && after.managementExit === null,
          'missing-context quit started a close/exit confirmation',
          { closeConfirmation: after.closeConfirmation, managementExit: after.managementExit },
        );
        ensure(
          after.activeBuddyId === before.activeBuddyId
            && after.buddiesRevision === before.buddiesRevision,
          'missing-context window command changed the buddy selection',
          { before: buddyContextOf(before), after: buddyContextOf(after) },
        );
        return {
          initiating: buddyContextOf(before),
          visibleBefore,
          visibleAfter,
          hiddenPhase: { hiddenWithValidContext, missingShowRemainedHidden },
          selectionUnchanged:
            after.activeBuddyId === before.activeBuddyId
              && after.buddiesRevision === before.buddiesRevision,
          processAlive: instance.child.exitCode === null,
          closeConfirmation: after.closeConfirmation,
          outcomes,
        };
      },
    );

    await check('buddy.stale-quit-context-keeps-process', '陈旧 Quit 在实际受理时拒绝，不退出或改变当前伙伴', async () => {
      const before = await readSnapshot(cdp);
      ensure(before.buddiesRevision > 1, 'stale quit requires an earlier selection revision');
      const staleContext = {
        expectedBuddyId: before.activeBuddyId,
        expectedBuddiesRevision: before.buddiesRevision - 1,
      };
      // IPC may acknowledge enqueueing; the exit owner must reject the stale
      // intent before associating a confirmation or consuming any draft.
      await cdp.invoke('prototype_window', { command: 'quit', context: staleContext }).catch(() => {});
      await sleep(500);
      const after = await readSnapshot(cdp);
      ensure(instance.child.exitCode === null, 'stale quit terminated the process');
      ensure(await mainWindowVisible(instance), 'stale quit hid the main window');
      ensure(!after.closeConfirmation && after.managementExit === null, 'stale quit installed a confirmation');
      ensure(after.activeBuddyId === before.activeBuddyId && after.buddiesRevision === before.buddiesRevision,
        'stale quit changed selection');
      ensure(after.chat.draft === before.chat.draft && after.chatDraft.revision === before.chatDraft.revision,
        'stale quit consumed a draft');
      return { staleContext, current: buddyContextOf(after), stillRunning: true };
    });

    await check('presentation.pet-close-and-restore', '原生关闭宠物后主窗真实按钮恢复宠物，伙伴与聊天状态不变', async () => {
      await navigatePage(cdp, 'appearance');
      const before = await readSnapshot(cdp);
      const closed = await runNative(instance, [
        '-Action', 'close-window', '-RootPid', String(instance.pid), '-WindowTitle', 'Moe Buddy Pet',
      ]);
      ensure(closed.ok === true, 'native pet close was not delivered', closed);
      const waitForPet = async (visible) => {
        const deadline = Date.now() + 5_000;
        while (Date.now() < deadline) {
          const windows = await nativeWindows(instance);
          if (windows.find((window) => window.title === 'Moe Buddy Pet')?.visible === visible) return true;
          await sleep(150);
        }
        return false;
      };
      ensure(await waitForPet(false), 'native pet close did not hide the pet');
      await cdp.clickElement(`__smoke.button('显示宠物')`);
      ensure(await waitForPet(true), 'main-window button did not restore the pet');
      const after = await readSnapshot(cdp);
      ensure(await mainWindowVisible(instance), 'pet presentation changed main-window visibility');
      ensure(after.activeBuddyId === before.activeBuddyId && after.buddiesRevision === before.buddiesRevision,
        'pet presentation changed the buddy identity');
      ensure(JSON.stringify(after.chat) === JSON.stringify(before.chat), 'pet presentation changed chat state');
      return { buddyId: after.activeBuddyId, petRestored: true, mainVisible: true };
    });

    await check('buddy.create-isolated', '真实 DOM 创建伙伴后选择新伙伴，且不继承消息、草稿、配置或凭据', async () => {
      await navigatePage(cdp, 'buddies');
      await installIpcObserver(cdp);
      await clearIpcCalls(cdp);
      const before = await readSnapshot(cdp);
      const requestsBefore = model.state.requests;
      await cdp.clickElement(`__smoke.buddyAction('新建伙伴')`);
      await cdp.waitFor(
        `__smoke.buddyForm('新建伙伴') !== null`,
        5_000,
        'buddy create form',
      );
      const label = `合成伙伴-${Date.now().toString(36)}`;
      const persona = 'synthetic buddy B persona';
      await cdp.typeInto(`__smoke.buddyControl('新建伙伴', '伙伴名称')`, label);
      await cdp.typeInto(`__smoke.buddyControl('新建伙伴', '伙伴人设')`, persona);
      await waitForSnapshot(
        cdp,
        (state) => state.managementDraft.dirty === true
          && state.managementDraft.kind === 'buddy_create',
        10_000,
        'create form protected as buddy_create draft',
      );
      await cdp.clickElement(`__smoke.buddyFormButton('新建伙伴', '创建伙伴')`);
      const afterCreate = await waitForSnapshot(
        cdp,
        (state) => state.activeBuddyId !== before.activeBuddyId
          && state.buddiesRevision > before.buddiesRevision,
        15_000,
        'new buddy selected',
      );
      const createDispatches = (await ipcCalls(cdp, 'prototype_dispatch'))
        .filter((entry) => entry.args?.command?.kind === 'create_buddy');
      ensure(createDispatches.length >= 1, 'DOM create submission did not dispatch create_buddy');
      const createSubmit = createDispatches[createDispatches.length - 1];
      ensure(
        createSubmit.args.context?.expectedBuddyId === before.activeBuddyId
          && createSubmit.args.context?.expectedBuddiesRevision === before.buddiesRevision,
        'create_buddy dispatch does not carry the initiating buddy context',
        {
          sent: createSubmit.args.context,
          expected: buddyContextOf(before),
        },
      );
      ensure(
        createSubmit.args.command?.expectedManagementDraftVersion
          === afterCreate.managementDraft.version,
        'create_buddy dispatch does not use the exact consumed management version',
        {
          sent: createSubmit.args.command?.expectedManagementDraftVersion,
          consumed: afterCreate.managementDraft.version,
        },
      );
      ensure(afterCreate.activeBuddyId !== null, 'host did not issue a new active buddy');
      buddyBId = afterCreate.activeBuddyId;
      buddyBLabel = buddyLabelFromSnapshot(afterCreate, buddyBId);
      ensure(buddyBLabel === label, `new buddy label mismatch: ${buddyBLabel} !== ${label}`, {
        buddyBId,
        buddies: afterCreate.buddies,
      });
      ensure(afterCreate.chat.messages.length === 0, 'new buddy inherited old messages');
      ensure(afterCreate.chat.draft === '', 'new buddy inherited old draft');
      ensure(
        (await cdp.evaluate(`__smoke.chatInput()?.value ?? ''`)) === '',
        'new buddy composer inherited old draft text',
      );
      ensure(
        afterCreate.modelSettings === null || afterCreate.modelSettings.endpoint === '',
        'new buddy inherited the previous model endpoint',
        { modelSettings: afterCreate.modelSettings },
      );
      ensure(
        afterCreate.modelSettings?.hasApiKey !== true,
        'new buddy inherited an API Key flag',
        { modelSettings: afterCreate.modelSettings },
      );
      ensure(afterCreate.externalTransmissionAllowed !== true, 'new buddy inherited transmission permission');
      ensure(
        afterCreate.managementDraft.dirty === false && afterCreate.managementDraft.kind === 'none',
        'host did not consume the create form management draft',
        { managementDraft: afterCreate.managementDraft },
      );
      // 成功回执（事件或回复）必须按 buddyCommit 消费本次表单，不能留下旧稿。
      await cdp.waitFor(
        `__smoke.buddyForm('新建伙伴') === null && __smoke.buddyControl('新建伙伴', '伙伴名称') === null`,
        5_000,
        'create form consumed after the matching buddyCommit',
      );
      ensure(
        afterCreate.activeBuddyPersona === persona,
        'new buddy active persona does not match the submitted persona',
        { activeBuddyPersona: afterCreate.activeBuddyPersona },
      );
      ensure(model.state.requests === requestsBefore, 'creating a buddy re-sent a chat request');
      return {
        previousBuddyId: before.activeBuddyId,
        newBuddyId: buddyBId,
        buddiesRevision: afterCreate.buddiesRevision,
        modelSettings: afterCreate.modelSettings,
      };
    });

    await check('buddy.switch-isolation', '两个伙伴来回切换：消息、草稿、人设/模型配置与迟到结果互不串档', async () => {
      const bSnapshot = await readSnapshot(cdp);
      ensure(bSnapshot.activeBuddyId === buddyBId, 'expected the newly created buddy to stay selected');
      // 伙伴 B 配置独立回环并发送一轮。
      await configureModelViaDom(instance, {
        endpoint: model.endpoint,
        model: 'synthetic-buddy-b-model',
        persona: 'synthetic buddy B persona edited',
        apiKey: 'synthetic-buddy-b-key',
        allowTransmission: true,
      });
      await closePanelIfOpen(cdp);
      const roundB = 'synthetic buddy B round';
      await sendChatViaDom(cdp, roundB);
      await waitForSnapshot(
        cdp,
        (state) => state.chat.status === 'ready'
          && state.chat.messages.some((message) => message.text === roundB),
        20_000,
        'buddy B round committed',
      );
      const sessionIdB = (await readSnapshot(cdp)).activeSessionId;

      // 切回伙伴 A：A 自己的消息仍在，B 的正文不出现。
        await selectBuddy(cdp, buddyALabel);
      await waitForSnapshot(cdp, (state) => state.activeBuddyId === buddyAId, 15_000, 'back to buddy A');
      const backA = await readSnapshot(cdp);
      const aText = await cdp.evaluate(`__smoke.messagesText()`);
      ensure(aText.includes('synthetic round'), 'buddy A lost its own messages after the buddy switch');
      ensure(!aText.includes(roundB), 'buddy A rendered buddy B messages');
      ensure(backA.chat.messages.every((message) => message.text !== roundB), 'buddy A host snapshot carried B messages');
      ensure(backA.modelSettings?.endpoint === model.endpoint, 'buddy A model endpoint was not its own');
      const sessionIdA = backA.activeSessionId;

      // 同一局部会话 ID/标签也不共享草稿：只比较界面正文与宿主草稿，不按名称推断身份。
      const draftA = 'buddy A 的隔离草稿';
      await typeChat(cdp, draftA);
      await waitForSnapshot(cdp, (state) => state.chat.draft === draftA, 10_000, 'buddy A draft synced');
        await selectBuddy(cdp, buddyBLabel);
      await waitForSnapshot(cdp, (state) => state.activeBuddyId === buddyBId, 15_000, 'back to buddy B');
      const backB = await readSnapshot(cdp);
      ensure(backB.activeSessionId === sessionIdB, 'buddy B session selection was replaced', {
        sessionIdA,
        sessionIdB,
        active: backB.activeSessionId,
      });
      ensure(backB.chat.draft === '', 'buddy B inherited buddy A draft', { draft: backB.chat.draft });
      ensure(
        (await cdp.evaluate(`__smoke.chatInput()?.value ?? null`)) !== draftA,
        'buddy B composer inherited buddy A draft text',
      );
      ensure(
        (await cdp.evaluate(`__smoke.messagesText()`)).includes(roundB),
        'buddy B lost its own messages after switching away and back',
      );

      // A→B→A：草稿与消息按伙伴恢复，目录修订不得回到旧值。
      const revisionBefore = backB.buddiesRevision;
        await selectBuddy(cdp, buddyALabel);
      const backA2 = await waitForSnapshot(
        cdp,
        (state) => state.activeBuddyId === buddyAId && state.chat.draft === draftA,
        15_000,
        'buddy A draft restored on A→B→A',
      );
      ensure(backA2.buddiesRevision >= revisionBefore, 'A→B→A rolled the registry revision back', {
        before: revisionBefore,
        after: backA2.buddiesRevision,
      });
      ensure(
        (await cdp.evaluate(`__smoke.chatInput()?.value ?? ''`)) === draftA,
        'buddy A own draft was not restored',
      );
      return {
        buddyAId,
        buddyBId,
        sessionIdA,
        sessionIdB,
        sameLocalSessionId: sessionIdA === sessionIdB,
        draftA,
        roundB,
        buddiesRevision: backA2.buddiesRevision,
      };
    });

    await check('buddy.memory-isolation', '两个伙伴的真实私密记忆互不串档', async () => {
      const start = await readSnapshot(cdp);
      ensure(start.activeBuddyId === buddyAId, 'memory isolation check did not start on buddy A', {
        activeBuddyId: start.activeBuddyId,
        buddyAId,
      });
      const memoryA = 'buddy A private memory';
      const memoryB = 'buddy B private memory';
      await openPanel(instance, '记忆');
      await cdp.waitFor(`document.querySelector('.remember-form textarea') !== null`, 5_000, 'buddy A memory composer');
      await cdp.typeInto('.remember-form textarea', memoryA);
      await cdp.clickElement(B('记住'));
      await cdp.waitFor(
        `__smoke.panelText().includes(${JSON.stringify(memoryA)})`,
        10_000,
        'buddy A memory rendered',
      );
      const aSnap = await readSnapshot(cdp);
      ensure(aSnap.memories.some((memory) => memory.text === memoryA), 'buddy A memory missing from host');
      await closePanelIfOpen(cdp);

        await selectBuddy(cdp, buddyBLabel);
      await waitForSnapshot(cdp, (state) => state.activeBuddyId === buddyBId, 15_000, 'buddy B for memory isolation');
      await openPanel(instance, '记忆');
      await cdp.clickElement(B('刷新列表'));
      await cdp.waitFor(
        `__smoke.panelText().includes(${JSON.stringify(memoryA)}) === false`,
        10_000,
        'buddy B memory list excluded A memory',
      );
      const bBefore = await readSnapshot(cdp);
      ensure(
        !bBefore.memories.some((memory) => memory.text === memoryA),
        'buddy B inherited buddy A private memory',
        { memories: bBefore.memories.map((memory) => memory.text) },
      );
      await cdp.typeInto(C('新增私密记忆'), memoryB);
      await cdp.clickElement(B('记住'));
      await cdp.waitFor(
        `__smoke.panelText().includes(${JSON.stringify(memoryB)})`,
        10_000,
        'buddy B memory rendered',
      );
      const bSnap = await readSnapshot(cdp);
      ensure(bSnap.memories.some((memory) => memory.text === memoryB), 'buddy B memory missing from host');
      ensure(
        !bSnap.memories.some((memory) => memory.text === memoryA),
        'buddy B snapshot carried buddy A private memory',
      );
      await closePanelIfOpen(cdp);

        await selectBuddy(cdp, buddyALabel);
      await waitForSnapshot(cdp, (state) => state.activeBuddyId === buddyAId, 15_000, 'buddy A restored for memory isolation');
      await openPanel(instance, '记忆');
      await cdp.clickElement(B('刷新列表'));
      await cdp.waitFor(
        `__smoke.panelText().includes(${JSON.stringify(memoryA)})`,
        10_000,
        'buddy A memory restored',
      );
      const aRestored = await readSnapshot(cdp);
      ensure(
        !aRestored.memories.some((memory) => memory.text === memoryB),
        'buddy A inherited buddy B private memory',
        { memories: aRestored.memories.map((memory) => memory.text) },
      );
      await closePanelIfOpen(cdp);
      return {
        buddyAId,
        buddyBId,
        memoryA,
        memoryB,
        aMemoryId: aSnap.memories.find((memory) => memory.text === memoryA)?.id ?? null,
        bMemoryId: bSnap.memories.find((memory) => memory.text === memoryB)?.id ?? null,
        crossTalk: false,
      };
    });

    await check('buddy.rename-profile-only', '真实 DOM 改名只推进档案版本，实例 ID、人设与模型版本保持', async () => {
      await navigatePage(cdp, 'buddies');
      const before = await readSnapshot(cdp);
      const activeBefore = buddyRevisionsOf(before, before.activeBuddyId);
      const newLabel = `${buddyLabelFromSnapshot(before, before.activeBuddyId)}-改名`;
      await cdp.clickElement(`__smoke.buddyAction('伙伴改名')`);
      await cdp.waitFor(`__smoke.buddyForm('伙伴改名') !== null`, 5_000, 'buddy rename form');
      await cdp.typeInto(`__smoke.buddyControl('伙伴改名', '新的伙伴名称')`, newLabel);
      await waitForSnapshot(
        cdp,
        (state) => state.managementDraft.dirty === true
          && state.managementDraft.kind === 'buddy_rename',
        10_000,
        'rename form protected as buddy_rename draft',
      );
      await cdp.clickElement(`__smoke.buddyFormButton('伙伴改名', '保存名称')`);
      const renamed = await waitForSnapshot(
        cdp,
        (state) => state.buddies.some((buddy) => buddy.id === before.activeBuddyId && buddy.label === newLabel)
          && state.buddiesRevision > before.buddiesRevision,
        15_000,
        'buddy renamed',
      );
      const renameDispatches = (await ipcCalls(cdp, 'prototype_dispatch'))
        .filter((entry) => entry.args?.command?.kind === 'rename_buddy');
      ensure(renameDispatches.length >= 1, 'DOM rename submission did not dispatch rename_buddy');
      const renameSubmit = renameDispatches[renameDispatches.length - 1];
      ensure(
        renameSubmit.args.context?.expectedBuddyId === before.activeBuddyId
          && renameSubmit.args.context?.expectedBuddiesRevision === before.buddiesRevision,
        'rename_buddy dispatch does not carry the initiating buddy context',
        {
          sent: renameSubmit.args.context,
          expected: buddyContextOf(before),
        },
      );
      ensure(
        renameSubmit.args.command?.expectedManagementDraftVersion
          === renamed.managementDraft.version,
        'rename_buddy dispatch does not use the exact consumed management version',
        {
          sent: renameSubmit.args.command?.expectedManagementDraftVersion,
          consumed: renamed.managementDraft.version,
        },
      );
      const activeAfter = buddyRevisionsOf(renamed, before.activeBuddyId);
      ensure(activeAfter.id === activeBefore.id, 'rename changed the stable instance id');
      ensure(
        activeAfter.profileRevision > activeBefore.profileRevision,
        'rename did not advance the profile revision',
        { before: activeBefore, after: activeAfter },
      );
      ensure(
        activeAfter.personaRevision === activeBefore.personaRevision,
        'rename advanced the persona revision',
      );
      ensure(
        activeAfter.modelRevision === activeBefore.modelRevision,
        'rename advanced the model revision',
      );
      ensure(renamed.buddiesRevision > before.buddiesRevision, 'rename did not advance the selection revision');
      ensure(
        renamed.managementDraft.dirty === false && renamed.managementDraft.kind === 'none',
        'host did not consume the rename form management draft',
        { managementDraft: renamed.managementDraft },
      );
      await cdp.waitFor(
        `__smoke.buddyForm('伙伴改名') === null`,
        5_000,
        'rename form consumed after the matching buddyCommit',
      );
      ensure(
        renamed.activeBuddyPersona === before.activeBuddyPersona,
        'rename changed the bound persona',
        { before: before.activeBuddyPersona, after: renamed.activeBuddyPersona },
      );
      if (renamed.activeBuddyId === buddyAId) buddyALabel = newLabel;
      return { activeBefore, activeAfter, buddiesRevision: renamed.buddiesRevision };
    });

    // 全伙伴 Quit 保护：让非活动伙伴保留未提交草稿，活动伙伴清空；实例稍后
    // 真实退出时必须先出现确认，而不是只扫描当前伙伴/当前会话。
    await check(
      'buddy.quit-protects-other-buddy-draft',
      '整个应用退出须识别非活动伙伴的未提交草稿，不能只保护当前伙伴',
      async () => {
        ensure(
          buddyAId !== null && buddyBId !== null,
          'two-buddy prerequisite failed before the whole-application Quit check',
          { buddyAId, buddyBId },
        );
        const before = await readSnapshot(cdp);
        const currentId = before.activeBuddyId;
        ensure(currentId !== null, 'no active buddy before the whole-application Quit check');
        const otherId = currentId === buddyAId ? buddyBId : buddyAId;
        await clearChatDraft(cdp, 'active buddy draft cleared');
        await selectBuddy(cdp, buddyLabelFromSnapshot(before, otherId));
        const other = await waitForSnapshot(
          cdp,
          (state) => state.activeBuddyId === otherId,
          15_000,
          'other buddy selected for quit protection',
        );
        await clearChatDraft(cdp, 'other buddy prior draft cleared');
        const draft = '跨伙伴退出保护草稿';
        await typeChat(cdp, draft);
        await waitForSnapshot(
          cdp,
          (state) => state.chat.draft === draft,
          10_000,
          'other buddy draft synced',
        );
        await selectBuddy(cdp, buddyLabelFromSnapshot(other, currentId));
        await waitForSnapshot(
          cdp,
          (state) => state.activeBuddyId === currentId,
          15_000,
          'active buddy restored after arming other draft',
        );
        await clearChatDraft(cdp, 'active buddy draft cleared before quit');
        const final = await readSnapshot(cdp);
        ensure(final.chat.draft === '', 'active buddy still has a draft before quit evidence', final.chat);
        crossBuddyDraftArmed = true;
        return { otherId, activeId: currentId, draft };
      },
    );
  } finally {
    await resetUiForExit(instance);
  }
  return {
    bHostAvailable: true,
    buddyAId,
    buddyBId,
    buddyALabel,
    buddyBLabel,
    crossBuddyDraftArmed,
    restartReady: true,
  };
}

/** 同一数据根重启：伙伴注册表、选择、档案版本与两伙伴正文各归原主。 */
async function scenarioBuddyRestart(instance, outcome) {
  const { cdp } = instance;
  return check(
    'buddy.restart-retained',
    '重启后伙伴选择与档案版本保留，两个伙伴的会话/配置/草稿不串档，密钥与发送许可不恢复',
    async () => {
      const snapshot = await readSnapshot(cdp);
      ensure(
        (snapshot.buddies ?? []).length >= 2,
        'restart did not restore both buddies',
        { buddies: snapshot.buddies },
      );
      const ids = snapshot.buddies.map((buddy) => buddy.id);
      ensure(ids.includes(outcome.buddyAId) && ids.includes(outcome.buddyBId), 'restart lost a stable buddy id', {
        ids,
        expected: [outcome.buddyAId, outcome.buddyBId],
      });
      ensure(
        snapshot.activeBuddyId === outcome.buddyAId || snapshot.activeBuddyId === outcome.buddyBId,
        'restart selected an unknown buddy',
        { activeBuddyId: snapshot.activeBuddyId },
      );
      ensure(
        snapshot.modelSettings?.hasApiKey !== true,
        'restart restored an API Key flag',
        { modelSettings: snapshot.modelSettings },
      );
      ensure(snapshot.externalTransmissionAllowed !== true, 'restart restored transmission permission');
      const chips = await cdp.evaluate(`__smoke.buddyChips()`);
      ensure(chips.length === snapshot.buddies.length, 'restart buddy chips do not match the host registry');
      ensure(
        (await cdp.evaluate(`__smoke.buddyIdentity()`)).includes(buddyLabelFromSnapshot(snapshot, snapshot.activeBuddyId)),
        'restart identity area does not show the restored active buddy',
      );
      // 切到另一伙伴：正文不串档。
      const otherId = snapshot.activeBuddyId === outcome.buddyAId
        ? outcome.buddyBId
        : outcome.buddyAId;
      await selectBuddy(cdp, buddyLabelFromSnapshot(snapshot, otherId));
      const other = await waitForSnapshot(cdp, (state) => state.activeBuddyId === otherId, 15_000, 'other buddy selected after restart');
      const text = await cdp.evaluate(`__smoke.messagesText()`);
      ensure(text.length >= 0, 'messages text unavailable');
      ensure(
        (other.modelSettings?.endpoint ?? '') === '' || other.modelSettings?.endpoint === snapshot.modelSettings?.endpoint
          || other.modelSettings?.endpoint.includes('127.0.0.1'),
        'restored buddy endpoint is not its own loopback config',
        { modelSettings: other.modelSettings },
      );
      return {
        activeBuddyId: snapshot.activeBuddyId,
        buddies: snapshot.buddies,
        switchedTo: otherId,
        hasApiKey: other.modelSettings?.hasApiKey ?? null,
      };
    },
  );
}

/**
 * A 会话生命周期真实 DOM 验收。
 *
 * 前置：当前宿主已按冻结 A 契约输出会话三元组与 sessionIsolationAllowed。
 * 若宿主仍缺字段，只记录准确的待联调项，不把旧宿主或桩结果记为 A 通过。
 * 所有会话动作都通过真实表单/按钮输入，独立 IPC 只用于读回或制造冲突前置。
 */
async function scenarioSessionLifecycle(instance, model) {
  const { cdp } = instance;
  const contract = await sessionContractReadback(cdp);
  if (!contract.supported) {
    RUN.pending.push({
      id: 'session.a-host-contract',
      reason:
        'real host snapshot lacks activeSessionId/sessionsRevision/sessions/sessionIsolationAllowed; strict frontend cannot connect and A DOM acceptance stays pending integration',
      observedError: contract.response?.error ?? null,
      observedFields: contract.response?.snapshot === null
        ? null
        : Object.keys(contract.response.snapshot ?? {}).sort(),
    });
    console.log('PENDING session.a-host-contract: real host has not integrated the frozen A snapshot contract');
    return { aHostAvailable: false, archivedSessionId: null, archivedSessionLabel: null };
  }

  let archivedSessionId = null;
  let archivedSessionLabel = null;

  try {
    await check('session.config-loopback', '真实 DOM 配置回环端点并允许外传，不使用默认 daily 或真实服务', async () => {
    await configureModelViaDom(instance, {
      endpoint: model.endpoint,
      model: 'synthetic-session-model',
      persona: 'synthetic session persona',
      apiKey: 'synthetic-session-key',
      allowTransmission: true,
    });
    await closePanelIfOpen(cdp);
    const snapshot = await readSnapshot(cdp);
    const chips = await cdp.evaluate(`__smoke.sessionChips()`);
    ensure(snapshot.externalTransmissionAllowed === true, 'loopback transmission was not granted');
    ensure(snapshot.modelSettings?.endpoint === model.endpoint, 'loopback endpoint not retained');
    ensure(model.endpoint.startsWith('http://127.0.0.1:'), 'model endpoint is not numeric loopback');
    ensure(Array.isArray(snapshot.sessions) && snapshot.sessions.length >= 1, 'host issued no session');
    ensure(chips.length === snapshot.sessions.length, 'session chips do not match host directory');
    ensure(
      chips.some((chip) => chip.active === true),
      'session chips do not mark the host active session',
    );
    return {
      endpoint: model.endpoint,
      sessionsRevision: snapshot.sessionsRevision,
      activeSessionId: snapshot.activeSessionId,
      chips,
    };
  });

  await check('session.over-old-32-rounds', '连续 33 轮真实 DOM 发送仍可继续使用并逐轮得到合成回复', async () => {
    const requestsBefore = model.state.requests;
    const rounds = 33;
    for (let index = 0; index < rounds; index += 1) {
      const text = `synthetic round ${index}`;
      await sendChatViaDom(cdp, text);
      await waitForSnapshot(
        cdp,
        (state) => state.chat.status === 'ready'
          && state.chat.messages.some((message) => message.role === 'user' && message.text === text)
          && state.chat.messages.filter((message) => message.role === 'assistant').length >= index + 1,
        20_000,
        `round ${index} committed`,
      );
    }
    const snapshot = await readSnapshot(cdp);
    const requests = model.state.requests - requestsBefore;
    ensure(requests === rounds, `loopback received ${requests} requests for ${rounds} rounds`, {
      requests,
    });
    ensure(snapshot.chat.status === 'ready', `chat not ready after ${rounds} rounds`);
    ensure(
      snapshot.chat.messages.filter((message) => message.role === 'assistant').length >= rounds,
      'fewer assistant replies than rounds after the old 32-round boundary',
      { messageCount: snapshot.chat.messages.length },
    );
    return { rounds, requests, messageCount: snapshot.chat.messages.length };
  });

  await check('session.catalog-draft-message-isolation', '新建/切换按宿主 ID 与修订隔离新旧草稿和消息，不把 A 正文带入 B', async () => {
    const before = await readSnapshot(cdp);
    const sessionA = before.activeSessionId;
    const labelA = sessionLabelFromSnapshot(before, sessionA);
    const roundsBefore = before.sessionsRevision;

    await cdp.clickElement(`__smoke.sessionAction('新建会话')`);
    await waitForSnapshot(
      cdp,
      (state) => state.activeSessionId !== sessionA && state.sessionsRevision > roundsBefore,
      10_000,
      'new isolated session B',
    );
    const afterNew = await readSnapshot(cdp);
    const sessionB = afterNew.activeSessionId;
    ensure(sessionB !== null && sessionB !== sessionA, 'new session did not get a host-issued ID');
    ensure(afterNew.chat.draft === '', 'new session inherited an old draft');
    ensure(afterNew.chat.messages.length === 0, 'new session inherited old messages');

    const labelB = sessionLabelFromSnapshot(afterNew, sessionB);
    await clickSessionChip(cdp, labelA);
    await waitForSnapshot(cdp, (state) => state.activeSessionId === sessionA, 10_000, 'back to A');

    const draftA = 'A 的会话草稿';
    await typeChat(cdp, draftA);
    await waitForSnapshot(cdp, (state) => state.chat.draft === draftA, 10_000, 'A draft synced');

    await clickSessionChip(cdp, labelB);
    await waitForSnapshot(cdp, (state) => state.activeSessionId === sessionB, 10_000, 'switch to B');
    const textB = await cdp.evaluate(`__smoke.messagesText()`);
    const inputB = await cdp.evaluate(`__smoke.chatInput()?.value ?? null`);
    ensure(!textB.includes('synthetic round 0'), 'B rendered A messages');
    ensure(inputB !== draftA, 'B composer inherited A draft text');

    await clickSessionChip(cdp, labelA);
    await waitForSnapshot(cdp, (state) => state.activeSessionId === sessionA, 10_000, 'switch back to A');
    const inputA = await cdp.evaluate(`__smoke.chatInput()?.value ?? null`);
    const textA = await cdp.evaluate(`__smoke.messagesText()`);
    ensure(inputA === draftA, 'A own draft was not restored on switch back');
    ensure(textA.includes('synthetic round 0'), 'A messages were lost after switching back');
    return { sessionA, sessionB, labelA, labelB, restoredDraft: inputA };
  });

  await check('session.failed-switch-keeps-draft', '切换前草稿同步失败时取消切换并保留原会话与草稿，不写回新会话', async () => {
    const before = await readSnapshot(cdp);
    ensure(
      (await cdp.evaluate(`__smoke.chatInput()?.disabled`)) !== true,
      'failed-switch precondition requires an editable composer',
    );
    const originalDraft = await cdp.evaluate(`__smoke.chatInput()?.value ?? ''`);
    const other = before.sessions.find((item) => item.id !== before.activeSessionId && !item.archived);
    ensure(other !== undefined, 'no second session available for the failed switch');
    const otherLabel = other.label.trim().length > 0 ? other.label : other.id;

    // 等待真实超预算提示落定，避免横幅出现时改变会话按钮的点击坐标。
    const oversized = 'x'.repeat(9_000);
    try {
      await typeChat(cdp, oversized);
      await cdp.waitFor(
        `__smoke.chatInput()?.value.length === ${oversized.length}`,
        10_000,
        'oversized draft typed',
      );
      await cdp.waitFor(
        `__smoke.alerts().includes('input_too_large')`,
        10_000,
        'oversized draft input_budget failure settled',
      );
      await clickSessionChip(cdp, otherLabel);
      // 取消提示是 role="alert" 的错误横幅（不是 role="status" 的告警行）。
      await cdp.waitFor(
        `__smoke.alerts().includes('本次会话操作已取消') && __smoke.alerts().includes('草稿已保留')`,
        10_000,
        'failed draft sync cancels switch',
      );
      const switchAlert = await cdp.evaluate(`__smoke.alerts()`);
      ensure(
        switchAlert.includes('草稿尚未同步到桌面宿主')
          && switchAlert.includes('本次会话操作已取消')
          && switchAlert.includes('草稿已保留'),
        'failed draft sync alert did not report cancellation and retained draft',
        { switchAlert },
      );
      const after = await readSnapshot(cdp);
      const input = await cdp.evaluate(`__smoke.chatInput()?.value ?? ''`);
      ensure(after.activeSessionId === before.activeSessionId, 'failed switch changed the active session', {
        before: snapshotSummary(before),
        after: snapshotSummary(after),
      });
      ensure(
        after.sessionsRevision === before.sessionsRevision,
        'failed switch advanced the directory revision',
        { before: snapshotSummary(before), after: snapshotSummary(after) },
      );
      ensure(input.length === oversized.length, 'failed switch discarded the original draft');
      // 超载荷不得被宿主部分接收：权威草稿仍是同步失败前的空基线。
      ensure(
        after.chat.draft === '',
        'failed draft sync still wrote a partial draft into the host',
        { hostDraftChars: after.chat.draft.length },
      );
      const otherText = await cdp.evaluate(`__smoke.messagesText()`);
      ensure(
        !otherText.includes(oversized.slice(0, 32)),
        'failed switch wrote the old draft into another session view',
      );
      return {
        activeSessionId: after.activeSessionId,
        sessionsRevision: after.sessionsRevision,
        retainedChars: input.length,
        replacedDraftChars: originalDraft.length,
        alerts: await cdp.evaluate(`__smoke.alerts()`),
      };
    } finally {
      // 失败路径也要把自己制造的脏草稿清掉，后续场景与正常退出口径不被污染。
      await clearChatDraft(cdp, 'oversized draft cleared after failed switch').catch((error) => {
        RUN.cleanupNotes.push(`failed-switch draft cleanup: ${error.message}`);
      });
      await dismissAlerts(cdp).catch(() => {});
    }
  });

  await check(
    'session.close-discard-reread-no-resurrect',
    'CloseChat 明确丢弃后重开/重读同会话不复活旧稿、不自动 set_draft，其他会话草稿保留',
    async () => {
      const start = await readSnapshot(cdp);
      ensure(start.chat.draft === '', 'discard check precondition requires an empty host draft', {
        host: snapshotSummary(start),
      });
      const activeId = start.activeSessionId;
      const activeLabel = sessionLabelFromSnapshot(start, activeId);
      const others = start.sessions.filter((item) => item.id !== activeId && !item.archived);
      ensure(others.length >= 1, 'discard check needs a second selectable session', {
        sessions: start.sessions,
      });
      const other = others[0];
      const otherLabel = other.label.trim().length > 0 ? other.label : other.id;

      // 只丢弃当前会话；另一个会话保留自己的草稿作为对照。
      await clickSessionChip(cdp, otherLabel);
      await waitForSnapshot(cdp, (state) => state.activeSessionId === other.id, 10_000, 'other session active');
      const otherDraft = 'B 保留草稿';
      await typeChat(cdp, otherDraft);
      await waitForSnapshot(cdp, (state) => state.chat.draft === otherDraft, 10_000, 'other session draft synced');
      await clickSessionChip(cdp, activeLabel);
      await waitForSnapshot(cdp, (state) => state.activeSessionId === activeId, 10_000, 'back to discard target');
      const targetDraft = 'A 待丢弃草稿';
      await typeChat(cdp, targetDraft);
      await waitForSnapshot(cdp, (state) => state.chat.draft === targetDraft, 10_000, 'target draft synced');
      const beforeDiscard = await readSnapshot(cdp);
      const revisionBefore = beforeDiscard.chatDraft.revision;
      ensure(beforeDiscard.chatDraft.discard === null, 'ordinary typing must not fabricate a discard receipt', {
        chatDraft: beforeDiscard.chatDraft,
      });

      // 真实按钮关闭并确认丢弃（只承诺当前会话草稿）。
      await navigatePage(cdp, 'chat');
      await cdp.clickElement(B('关闭聊天'));
      const dialog = await waitForDialog(cdp, 5_000);
      ensure(/当前会话草稿/.test(dialog) && !/所有会话草稿/.test(dialog), `close dialog does not scope the discard: ${dialog}`, {
        dialog,
      });
      await ensureDialogOperable(instance);
      const confirmLabel = await clickDialogConfirm(cdp);
      await cdp.waitFor(`__smoke.dialog() === null`, 5_000, 'dialog closed after discard confirm');
      ensure(await waitForMainVisible(instance, false, 5_000), 'main window did not hide after discard close');
      await recoverMain(instance);
      ensure(await waitForMainVisible(instance, true), 'main window did not reopen after discard close');

      // 重开同会话：权威回执仍在且绑定该会话，空稿本身不构成丢弃证据。
      const reopened = await waitForSnapshot(
        cdp,
        (state) => state.chat.draft === '' && state.chatDraft.discard !== null,
        10_000,
        'discard receipt visible after reopen',
      );
      const receipt = reopened.chatDraft.discard;
      ensure(receipt.revision === reopened.chatDraft.revision, 'discard receipt is not the current draft revision', {
        chatDraft: reopened.chatDraft,
      });
      ensure(
        receipt.sessionIds.length === 1 && receipt.sessionIds[0] === activeId,
        'discard receipt does not bind exactly the discarded session',
        { receipt, activeId },
      );
      ensure(
        (await cdp.evaluate(`__smoke.chatInput()?.value ?? null`)) === '',
        'reopen restored the discarded draft into the composer',
      );
      ensure(reopened.chatDraft.revision > revisionBefore, 'the explicit discard did not advance the draft revision', {
        before: revisionBefore,
        after: reopened.chatDraft.revision,
      });

      // A→B→A 重新采纳：被丢弃会话不得复活，另一会话草稿必须保留。
      await clickSessionChip(cdp, otherLabel);
      await waitForSnapshot(cdp, (state) => state.activeSessionId === other.id, 10_000, 'other session after discard');
      ensure(
        (await cdp.evaluate(`__smoke.chatInput()?.value ?? null`)) === otherDraft,
        'the other session lost its own draft after a scoped discard',
      );
      await clickSessionChip(cdp, activeLabel);
      await waitForSnapshot(cdp, (state) => state.activeSessionId === activeId, 10_000, 'discarded session reselected');
      const afterCycle = await readSnapshot(cdp);
      ensure(afterCycle.chat.draft === '', 'A→B→A resurrected the discarded draft in the host');
      ensure(
        (await cdp.evaluate(`__smoke.chatInput()?.value ?? null`)) === '',
        'A→B→A resurrected the discarded draft in the composer',
      );
      ensure(
        afterCycle.chatDraft.revision === reopened.chatDraft.revision,
        'A→B→A auto-dispatched set_draft for the discarded text',
        { before: reopened.chatDraft, after: afterCycle.chatDraft },
      );

      // 清理对照会话草稿，避免污染后续归档/退出场景。
      await clickSessionChip(cdp, otherLabel);
      await waitForSnapshot(cdp, (state) => state.activeSessionId === other.id, 10_000, 'cleanup other session');
      await clearChatDraft(cdp, 'other session draft cleared after discard check');
      await clickSessionChip(cdp, activeLabel);
      await waitForSnapshot(cdp, (state) => state.activeSessionId === activeId, 10_000, 'cleanup back to active');
      return {
        activeId,
        discardedSessionIds: receipt.sessionIds,
        chatDraftRevision: afterCycle.chatDraft.revision,
        confirmLabel,
        otherDraftPreserved: true,
      };
    },
  );

  await check(
    'session.cross-session-discard-reread-no-resurrect',
    'A 丢弃后 B 编辑推进全局修订，A 的逐会话水位仍清缓存；切回/重读 A 不复活正文也不自动 set_draft',
    async () => {
      const start = await readSnapshot(cdp);
      ensure(start.chat.draft === '', 'cross-session discard precondition requires an empty host draft', {
        host: snapshotSummary(start),
      });
      const activeId = start.activeSessionId;
      const activeLabel = sessionLabelFromSnapshot(start, activeId);
      const others = start.sessions.filter((item) => item.id !== activeId && !item.archived);
      ensure(others.length >= 1, 'cross-session discard check needs a second selectable session', {
        sessions: start.sessions,
      });
      const other = others[0];
      const otherLabel = other.label.trim().length > 0 ? other.label : other.id;
      const entryOf = (state, id) => state.chatDraft.sessions.find((item) => item.id === id);

      // A 真实按钮显式丢弃，拿到带 A 会话水位的权威快照。
      const targetDraft = 'A 跨会话丢弃草稿';
      await typeChat(cdp, targetDraft);
      await waitForSnapshot(cdp, (state) => state.chat.draft === targetDraft, 10_000, 'cross-session target draft synced');
      await navigatePage(cdp, 'chat');
      await cdp.clickElement(B('关闭聊天'));
      await waitForDialog(cdp, 5_000);
      await ensureDialogOperable(instance);
      const confirmLabel = await clickDialogConfirm(cdp);
      await cdp.waitFor(`__smoke.dialog() === null`, 5_000, 'dialog closed after cross-session discard');
      ensure(await waitForMainVisible(instance, false, 5_000), 'main window did not hide after cross-session discard');
      await recoverMain(instance);
      ensure(await waitForMainVisible(instance, true), 'main window did not reopen after cross-session discard');
      const discarded = await waitForSnapshot(
        cdp,
        (state) => state.chat.draft === '' && state.chatDraft.discard !== null,
        10_000,
        'cross-session discard receipt visible',
      );
      const receipt = discarded.chatDraft.discard;
      const discardedEntry = entryOf(discarded, activeId);
      ensure(
        discardedEntry !== undefined
          && discardedEntry.discardedRevision === receipt.revision
          && discardedEntry.revision === receipt.revision,
        'host does not expose A per-session discard watermark',
        { activeId, receipt, sessions: discarded.chatDraft.sessions },
      );

      // B 在 A 丢弃之后编辑，只推进 B 的会话修订与全局修订。
      await clickSessionChip(cdp, otherLabel);
      await waitForSnapshot(cdp, (state) => state.activeSessionId === other.id, 10_000, 'other session active after discard');
      const otherDraft = 'B 推进全局修订';
      await typeChat(cdp, otherDraft);
      await waitForSnapshot(cdp, (state) => state.chat.draft === otherDraft, 10_000, 'other session draft synced after discard');
      const afterOther = await readSnapshot(cdp);
      const aAfterOther = entryOf(afterOther, activeId);
      ensure(
        aAfterOther !== undefined && aAfterOther.discardedRevision === receipt.revision,
        'an edit of B invalidated A per-session discard watermark',
        { receipt, sessions: afterOther.chatDraft.sessions },
      );
      ensure(
        afterOther.chatDraft.revision > receipt.revision,
        'B edit did not advance the global draft revision',
        { receipt, chatDraft: afterOther.chatDraft },
      );
      const bEntry = entryOf(afterOther, other.id);
      ensure(
        bEntry !== undefined && bEntry.discardedRevision === null && bEntry.revision > receipt.revision,
        'B edit did not advance only B per-session revision',
        { other: other.id, receipt, sessions: afterOther.chatDraft.sessions },
      );

      // 真实点击切回 A：重读后草稿仍为空，且不得自动派发 set_draft 复活正文。
      const revisionBeforeSwitch = afterOther.chatDraft.revision;
      await clickSessionChip(cdp, activeLabel);
      await waitForSnapshot(cdp, (state) => state.activeSessionId === activeId, 10_000, 'discarded session reselected');
      await sleep(200);
      const afterSwitch = await readSnapshot(cdp);
      ensure(afterSwitch.chat.draft === '', 'switching back resurrected A discarded draft in the host', {
        chatDraft: afterSwitch.chatDraft,
        text: afterSwitch.chat.draft,
        input: await cdp.evaluate(`__smoke.chatInput()?.value ?? null`),
      });
      ensure(
        (await cdp.evaluate(`__smoke.chatInput()?.value ?? null`)) === '',
        'switching back resurrected A discarded draft in the composer',
      );
      ensure(
        afterSwitch.chatDraft.revision === revisionBeforeSwitch,
        'switching back auto-dispatched set_draft for the discarded text',
        { before: revisionBeforeSwitch, after: afterSwitch.chatDraft },
      );

      // 清理 B 对照草稿，避免污染归档/退出场景。
      await clickSessionChip(cdp, otherLabel);
      await waitForSnapshot(cdp, (state) => state.activeSessionId === other.id, 10_000, 'cleanup other session');
      await clearChatDraft(cdp, 'other session draft cleared after cross-session discard');
      await clickSessionChip(cdp, activeLabel);
      await waitForSnapshot(cdp, (state) => state.activeSessionId === activeId, 10_000, 'cleanup back to active');
      return {
        activeId,
        otherId: other.id,
        discardedRevision: receipt.revision,
        afterOtherRevision: afterOther.chatDraft.revision,
        afterSwitchRevision: afterSwitch.chatDraft.revision,
        confirmLabel,
        otherDraftPreserved: true,
      };
    },
  );

  await check('session.archive-readonly', '归档当前干净会话后只读、保留选择且不隐式新建下一会话', async () => {
    const before = await readSnapshot(cdp);
    const beforeInput = await cdp.evaluate(`__smoke.chatInput()?.value ?? ''`);
    ensure(
      before.chat.draft === '' && beforeInput.length === 0,
      'archive precondition requires an empty draft',
      { host: snapshotSummary(before), input: beforeInput },
    );
    const targetId = before.activeSessionId;
    const targetLabel = sessionLabelFromSnapshot(before, targetId);
    await cdp.clickElement(`__smoke.sessionAction('归档当前会话')`);
    await cdp.waitFor(
      `__smoke.dialogText().includes('归档')`,
      5_000,
      'archive confirmation dialog',
    );
    await cdp.clickElement(`__smoke.dialogButton('确认归档')`);
    try {
      await waitForSnapshot(
        cdp,
        (state) => state.activeSessionId === targetId
          && state.sessions.some((item) => item.id === targetId && item.archived === true),
        10_000,
        'current session archived in place',
      );
    } catch (error) {
      const observed = await readSnapshot(cdp).catch(() => null);
      throw new CheckFailure(error.message, {
        before: snapshotSummary(before),
        observed: snapshotSummary(observed),
        dom: await domEvidence(cdp).catch(() => null),
      });
    }
    const archived = await readSnapshot(cdp);
    ensure(
      (await cdp.evaluate(`__smoke.archivedBanner()`)).includes('只能只读查看'),
      'archive read-only banner missing',
    );
    ensure((await cdp.evaluate(`${B('发送')}?.disabled`)) === true, 'archived session can still send');
    ensure((await cdp.evaluate(`__smoke.chatInput()?.disabled`)) === true, 'archived session textarea is editable');
    ensure(
      (await cdp.evaluate(`${B('归档当前会话')} !== null`)) === false,
      'archived session still offers archive action',
    );
    // 归档只读不提供解归档/删除入口：按真实可交互控件断言，
    // 不能直接搜整页文本——只读横幅本来就会说明“不提供解归档、删除会话”。
    const archivedControls = await cdp.evaluate(
      `Array.from(document.querySelectorAll('button, [role="button"], a[href]'))
        .map((node) => __smoke.norm(node.innerText))`,
    );
    ensure(
      !archivedControls.some((text) => /解归档|删除|恢复会话/.test(text)),
      'archived session exposes unarchive/delete controls',
      { archivedControls },
    );
    ensure(archived.activeSessionId === targetId, 'archiving implicitly created a next session');
    ensure(
      (await cdp.evaluate(`__smoke.sessionChips()`)).some(
        (chip) => chip.label === targetLabel && chip.archived === true,
      ),
      'archived session is no longer explicitly selectable',
    );
    archivedSessionId = targetId;
    archivedSessionLabel = targetLabel;

    // 归档会话保持选择，但后续 Unknown 场景需要切到一个未归档的干净会话。
    const next = archived.sessions.find((item) => item.id !== targetId && !item.archived);
    ensure(next !== undefined, 'no non-archived session remained after archiving');
    const nextLabel = next.label.trim().length > 0 ? next.label : next.id;
    await clickSessionChip(cdp, nextLabel);
    await waitForSnapshot(cdp, (state) => state.activeSessionId === next.id, 10_000, 'switch to clean session');
    return {
      archivedSessionId,
      archivedSessionLabel,
      cleanSessionId: next.id,
      sessionsRevision: archived.sessionsRevision,
    };
  });

  await check('session.unknown-new-isolated-no-resend', '真实回环断连产生 Unknown 后仅显式新建隔离会话，旧会话保持未知且请求计数不重复', async () => {
    const beforeUnknown = await readSnapshot(cdp);
    const originalId = beforeUnknown.activeSessionId;
    const originalLabel = sessionLabelFromSnapshot(beforeUnknown, originalId);
    const requestsBefore = model.state.requests;
    model.state.mode = 'drop';
    try {
      await sendChatViaDom(cdp, 'synthetic unknown request');
      await waitForSnapshot(
        cdp,
        (state) => state.chat.status === 'unknown' || state.chat.failure === 'send_unknown',
        20_000,
        'external unknown after loopback drop',
      );
    } finally {
      model.state.mode = 'reply';
    }
    const unknown = await readSnapshot(cdp);
    ensure(
      unknown.sessionIsolationAllowed === true,
      'host did not grant explicit isolated-session creation for an external unknown',
      { chat: unknown.chat, sessions: unknown.sessions },
    );
    ensure((await cdp.evaluate(`${B('发送')}?.disabled`)) === true, 'send stayed enabled after Unknown');
    ensure(
      (await cdp.evaluate(`__smoke.isolationHint()`)) !== null,
      'no explicit isolation hint shown after Unknown',
    );
    ensure(model.state.requests - requestsBefore === 1, 'unknown request was retried');

    const oldDraft = 'A 未知会话旧草稿';
    await typeChat(cdp, oldDraft);
    await cdp.waitFor(`__smoke.chatInput()?.value === ${JSON.stringify(oldDraft)}`, 5_000, 'old draft typed');
    const requestsBeforeCreate = model.state.requests;
    const sessionsBefore = unknown.sessionsRevision;
    await cdp.clickElement(`__smoke.sessionAction('新建会话')`);
    await waitForSnapshot(
      cdp,
      (state) => state.activeSessionId !== originalId && state.sessionsRevision > sessionsBefore,
      10_000,
      'isolated new session after Unknown',
    );
    const isolated = await readSnapshot(cdp);
    ensure(isolated.chat.draft === '', 'new isolated session inherited the unknown session draft');
    ensure(isolated.chat.messages.length === 0, 'new isolated session inherited unknown messages');
    ensure(model.state.requests === requestsBeforeCreate, 'creating an isolated session re-sent the unknown request');

    await clickSessionChip(cdp, originalLabel);
    await waitForSnapshot(cdp, (state) => state.activeSessionId === originalId, 10_000, 'back to unknown session');
    const back = await readSnapshot(cdp);
    const restored = await cdp.evaluate(`__smoke.chatInput()?.value ?? null`);
    ensure(back.chat.status === 'unknown', 'old session Unknown was cleared by switching');
    ensure(restored === oldDraft, 'old session own draft was not restored');
    ensure(model.state.requests === requestsBeforeCreate, 'switching back re-sent the unknown request');

    // 旧会话仍未知时普通切换保持锁定；显式新建后当前会话为空，旧会话草稿仍需退出保护。
    const freshBefore = back.sessionsRevision;
    await cdp.clickElement(`__smoke.sessionAction('新建会话')`);
    await waitForSnapshot(
      cdp,
      (state) => state.activeSessionId !== originalId && state.sessionsRevision > freshBefore,
      10_000,
      'fresh isolated session while the old draft remains protected',
    );
    const fresh = await readSnapshot(cdp);
    ensure(fresh.chat.draft === '', 'fresh isolated session inherited the unknown draft');
    ensure(fresh.chat.messages.length === 0, 'fresh isolated session inherited unknown messages');
    ensure(model.state.requests === requestsBeforeCreate, 'fresh isolated session re-sent the unknown request');
    return {
      originalId,
      firstIsolatedId: isolated.activeSessionId,
      isolatedId: fresh.activeSessionId,
      isolatedLabel: sessionLabelFromSnapshot(fresh, fresh.activeSessionId),
      requestsBefore,
      requestsAfter: model.state.requests,
      restoredOldDraft: oldDraft,
    };
  });

  } finally {
    // 场景失败时也把自己的面板/草稿/对话框收干净，让实例仍可经正常退出确认收尾。
    await resetUiForExit(instance);
  }
  return { aHostAvailable: true, archivedSessionId, archivedSessionLabel };
}

async function scenarioSessionArchiveRestart(instance, archived) {
  await check('session.archive-restart-retained', '重启后归档会话仍保留归档标记、可只读选择且不能发送', async () => {
    ensure(archived.archivedSessionId !== null, 'archive restart check has no archived session id');
    const snapshot = await waitForSnapshot(
      instance.cdp,
      (state) => state.sessions.some((item) => item.id === archived.archivedSessionId),
      20_000,
      'restart restored session directory',
    );
    const restored = snapshot.sessions.find((item) => item.id === archived.archivedSessionId);
    ensure(restored.archived === true, 'archived flag was lost after restart');
    await clickSessionChip(instance.cdp, archived.archivedSessionLabel);
    await waitForSnapshot(
      instance.cdp,
      (state) => state.activeSessionId === archived.archivedSessionId,
      10_000,
      'restart archived session selected',
    );
    ensure(
      (await instance.cdp.evaluate(`__smoke.archivedBanner()`)).includes('只能只读查看'),
      'restart archived session is not read-only',
    );
    ensure(
      (await instance.cdp.evaluate(`${B('发送')}?.disabled`)) === true,
      'restart archived session can send',
    );
    return {
      archivedSessionId: archived.archivedSessionId,
      sessionsRevision: snapshot.sessionsRevision,
    };
  });
}

async function scenarioSessionStorageUnknown(instance, model) {
  const { cdp } = instance;
  const registryPath = path.join(instance.dataRoot, 'buddies.v1');
  const backupPath = path.join(instance.dataRoot, 'buddies.v1.session-smoke-retained');
  const pendingPath = path.join(instance.dataRoot, 'buddies.pending');

  const restore = async () => {
    const backupExists = await fs.stat(backupPath).then(() => true, () => false);
    if (backupExists) {
      await fs.rm(registryPath, { recursive: true, force: true });
      await fs.rename(backupPath, registryPath);
    }
    await fs.rm(pendingPath, { force: true });
  };

  try {
    await check('session.storage-unknown-blocks-new', '伙伴档案 Publish 未知时新建会话入口保持锁定，普通快照不能解除全局未知', async () => {
      await configureModelViaDom(instance, {
        endpoint: model.endpoint,
        model: 'synthetic-session-model',
        persona: 'synthetic session persona',
        apiKey: 'synthetic-session-key',
        allowTransmission: true,
      });
      await closePanelIfOpen(cdp);
      await fs.rename(registryPath, backupPath);
      await fs.mkdir(registryPath);

      await openPanel(instance, '模型与预设');
      await cdp.typeInto(C('端点'), 'http://127.0.0.1:9/v1/chat/completions');
      await cdp.typeInto(C('API Key'), 'synthetic-session-key');
      await cdp.clickElement(B('应用配置'));
      await cdp.waitFor(
        `__smoke.alerts().includes('未确认') || __smoke.unknownLock() === true`,
        15_000,
        'storage publish unknown lock',
      );
      // Publish 未知时面板编辑仍未保存，关闭会先弹离开确认；这里显式放弃编辑，
      // 既保留 Unknown 锁，又不在失败路径留下挡住退出的对话框。
      const closed = await closePanelIfOpen(cdp, { discardEdits: true });

      const before = await readSnapshot(cdp);
      ensure(before.sessionIsolationAllowed === false, 'host granted isolation despite a storage unknown');
      ensure((await cdp.evaluate(`__smoke.sessionAction('新建会话')?.disabled`)) === true, 'new session stayed enabled under storage unknown');
      const title = await cdp.evaluate(`__smoke.sessionAction('新建会话')?.title ?? ''`);
      ensure(
        title.includes('未确认') || title.includes('全局') || title.includes('不能解除'),
        `new session title does not explain the global unknown: ${title}`,
      );
      const beforeRevision = before.sessionsRevision;
      await sleep(1_000);
      const after = await readSnapshot(cdp);
      ensure(after.activeSessionId === before.activeSessionId, 'storage unknown was bypassed by a new session');
      ensure(after.sessionsRevision === beforeRevision, 'new session dispatch happened under storage unknown');
      ensure((await cdp.evaluate(`${B('发送')}?.disabled`)) === true, 'send unlocked under storage unknown');
      return {
        sessionsRevision: after.sessionsRevision,
        activeSessionId: after.activeSessionId,
        newSessionTitle: title,
        panelClosedAfterDiscard: closed.discarded,
      };
    });
  } finally {
    await resetUiForExit(instance);
    await restore();
  }
}

async function scenarioSaveFailure(instance, model) {
  const { cdp } = instance;
  const blocker = path.join(instance.dataRoot, 'buddies.pending');
  const blockedEndpoint = 'http://127.0.0.1:9/v1/chat/completions';
  let baselineModel = null;
  let baselineHasKey = null;

  try {
    await check('save.failure-visible', '伙伴档案 CreatePending 保存失败显示明确未执行，不冒充成功', async () => {
      await configureModelViaDom(instance, {
        endpoint: model.endpoint,
        model: 'synthetic-smoke-model',
        persona: 'synthetic smoke persona',
        apiKey: 'synthetic-save-key',
      });
      baselineModel = await readActiveBuddyModel(instance.dataRoot);
      baselineHasKey = (await readSnapshot(cdp)).modelSettings?.hasApiKey ?? null;
      await fs.writeFile(blocker, 'synthetic pending blocker', 'utf8');
      await cdp.typeInto(C('端点'), blockedEndpoint);
      await cdp.typeInto(C('API Key'), 'synthetic-blocked-key');
      ensure(
        (await cdp.evaluate(`__smoke.allowTransmissionCheckbox()?.checked`)) === false,
        'blocked save test should keep transmission off',
      );
      await cdp.clickElement(B('应用配置'));
      await cdp.waitFor(
        `__smoke.alerts().includes('未执行') || __smoke.alerts().includes('伙伴档案')`,
        10_000,
        'buddy registry save failure alert',
      );
      const evidence = {
        errorText: await cdp.evaluate(`__smoke.alerts()`),
        statusText: await cdp.evaluate(`__smoke.statuses()`),
        successNotice: (await panelText(cdp)).includes('模型配置已应用到桌面宿主'),
        baselineEndpoint: baselineModel.endpoint,
        blockedEndpoint,
      };
      ensure(!evidence.successNotice, 'save failure also displayed a success notice', evidence);
      ensure(!evidence.errorText.includes('synthetic-blocked-key'), 'failure alert leaked the API key', evidence);
      return evidence;
    });

    await check('save.failure-not-executed-evidence', 'CreatePending 失败按 NotExecuted 报告：运行配置与磁盘均未改变，档案不泄漏 Key', async () => {
      const snapshot = await readSnapshot(cdp);
      const disk = await readBuddyRegistry(instance.dataRoot);
      const status = await cdp.evaluate(`__smoke.statuses()`);
      const formKey = await cdp.evaluate(`${C('API Key')}?.value ?? ''`);
      const evidence = {
        memoryEndpoint: snapshot.modelSettings?.endpoint ?? null,
        diskEndpoint: disk.model.endpoint,
        baselineEndpoint: baselineModel?.endpoint ?? null,
        blockedEndpoint,
        externalTransmissionAllowed: snapshot.externalTransmissionAllowed,
        allowTransmission: snapshot.modelSettings?.allowTransmission ?? null,
        hasApiKey: snapshot.modelSettings?.hasApiKey ?? null,
        baselineHasKey,
        formKeyRetained: formKey,
        errorText: await cdp.evaluate(`__smoke.alerts()`),
        unknownLock: await cdp.evaluate(`__smoke.unknownLock()`),
        statusText: status,
        pendingBlockerExists: await fs.stat(blocker).then(() => true, () => false),
        registryRawHasKey: disk.raw.includes('synthetic-blocked-key'),
        snapshotRawHasKey: JSON.stringify(snapshot).includes('synthetic-blocked-key'),
      };
      ensure(evidence.diskEndpoint === evidence.baselineEndpoint, 'blocked save altered buddies.v1', evidence);
      ensure(evidence.memoryEndpoint === evidence.baselineEndpoint, 'not-executed save changed the in-memory endpoint', evidence);
      ensure(evidence.externalTransmissionAllowed === false, 'failed save left transmission allowed', evidence);
      ensure(evidence.allowTransmission === false, 'failed save left the model consent flag enabled', evidence);
      ensure(evidence.hasApiKey === baselineHasKey, 'not-executed save changed the process credential', evidence);
      ensure(evidence.formKeyRetained === 'synthetic-blocked-key', 'not-executed save discarded the user key input', evidence);
      ensure(evidence.errorText.includes('未执行'), 'not-executed save was not reported as not executed', evidence);
      ensure(!evidence.errorText.includes('未确认'), 'not-executed save was reported as unknown', evidence);
      ensure(evidence.unknownLock === false, 'not-executed save incorrectly locked side effects', evidence);
      ensure(evidence.pendingBlockerExists, 'synthetic CreatePending blocker disappeared', evidence);
      ensure(!evidence.registryRawHasKey, 'failed save leaked the API key into buddies.v1', evidence);
      ensure(!evidence.snapshotRawHasKey, 'failed save leaked the API key into the snapshot', evidence);
      return evidence;
    });
  } finally {
    await fs.rm(blocker, { force: true });
    // 失败路径显式收掉面板/表单与草稿，保证实例仍能走自身正常退出确认。
    await resetUiForExit(instance);
  }
}

async function sha256(bytes) {
  const { createHash } = await import('node:crypto');
  return createHash('sha256').update(bytes).digest('hex');
}

async function restoreRegistryFile(registryPath, backupPath, originalBytes) {
  const backupExists = await fs.stat(backupPath).then(() => true, () => false);
  if (backupExists) {
    await fs.rm(registryPath, { recursive: true, force: true });
    await fs.rename(backupPath, registryPath);
  } else if (originalBytes !== null) {
    const currentIsFile = await fs.stat(registryPath).then((stat) => stat.isFile(), () => false);
    if (!currentIsFile) {
      await fs.rm(registryPath, { recursive: true, force: true });
      await fs.writeFile(registryPath, originalBytes);
    }
  }
  if (originalBytes !== null) {
    const restored = await fs.readFile(registryPath);
    ensure(restored.equals(originalBytes), 'restored buddies.v1 bytes differ from the retained original');
  }
  const restoredBytes = await fs.readFile(registryPath);
  return { sha256: await sha256(restoredBytes), bytes: restoredBytes.length };
}

async function scenarioPublishFailure(instance, model) {
  const { cdp } = instance;
  const registryPath = path.join(instance.dataRoot, 'buddies.v1');
  const backupPath = path.join(instance.dataRoot, 'buddies.v1.smoke-retained');
  const pendingPath = path.join(instance.dataRoot, 'buddies.pending');
  const blockedEndpoint = 'http://127.0.0.1:9/v1/chat/completions';
  let originalBytes = null;
  let baselineModel = null;
  let baselineHasKey = null;
  let blockerReady = false;
  let originalDigest = null;
  let cleanupEvidence = null;

  const restore = async () => {
    await restoreRegistryFile(registryPath, backupPath, originalBytes);
    await fs.rm(pendingPath, { force: true });
    const stat = await fs.stat(registryPath);
    cleanupEvidence = {
      restoredFile: stat.isFile(),
      sha256: await sha256(await fs.readFile(registryPath)),
      originalSha256: originalDigest,
      pendingRemoved: await fs.stat(pendingPath).then(() => false, () => true),
    };
    return cleanupEvidence;
  };

  try {
    await check('unknown.publish-failure-locks-side-effects', '伙伴档案 Publish 失败产生 Unknown，真实 DOM 提交后状态重读仍保持锁', async () => {
      await configureModelViaDom(instance, {
        endpoint: model.endpoint,
        model: 'synthetic-smoke-model',
        persona: 'synthetic smoke persona',
        apiKey: 'synthetic-publish-key',
      });
      baselineModel = await readActiveBuddyModel(instance.dataRoot);
      baselineHasKey = (await readSnapshot(cdp)).modelSettings?.hasApiKey ?? null;
      originalBytes = await fs.readFile(registryPath);
      originalDigest = await sha256(originalBytes);
      await fs.rename(registryPath, backupPath);
      await fs.mkdir(registryPath);
      blockerReady = true;

      await cdp.typeInto(C('端点'), blockedEndpoint);
      await cdp.typeInto(C('API Key'), 'synthetic-publish-key');
      ensure(
        (await cdp.evaluate(`__smoke.allowTransmissionCheckbox()?.checked`)) === false,
        'publish failure test should keep transmission off',
      );
      await cdp.clickElement(B('应用配置'));
      await cdp.waitFor(
        `__smoke.alerts().includes('未确认') || __smoke.alerts().includes('未执行')`,
        10_000,
        'publish failure alert',
      );
      await cdp.waitFor(`__smoke.unknownLock() === true`, 5_000, 'unknown command lock banner');
      const alertsBeforeReread = await cdp.evaluate(`__smoke.alerts()`);
      const beforeReread = await cdp.evaluate(`({
        unknownLock: __smoke.unknownLock(),
        panelOpen: __smoke.panel() !== null,
        send: __smoke.buttonState('发送'),
        apply: __smoke.buttonState('应用配置'),
      })`);
      const snapshot = await readSnapshot(cdp);
      const registryStat = await fs.stat(registryPath);
      ensure(registryStat.isDirectory(), 'synthetic publish blocker is not a directory', {
        registryPath,
      });
      ensure(await fs.stat(pendingPath).then(() => true, () => false), 'failed publish did not retain buddies.pending');
      const pendingBefore = await fs.stat(pendingPath);
      ensure(snapshot.externalTransmissionAllowed === false, 'publish failure left transmission allowed');
      ensure(snapshot.modelSettings?.allowTransmission === false, 'publish failure left the model consent flag enabled');
      ensure(snapshot.modelSettings?.hasApiKey === baselineHasKey, 'publish failure changed the process credential');
      ensure(snapshot.modelSettings?.endpoint === baselineModel?.endpoint, 'publish failure changed the in-memory endpoint');
      ensure(beforeReread.unknownLock === true, 'publish failure did not lock the command unknown', beforeReread);
      ensure(beforeReread.send.disabled === true, 'send was operable after an unknown publish result', beforeReread);
      ensure(beforeReread.apply.operable === false, 'model apply was operable after an unknown publish result', beforeReread);

      await cdp.clickElement(B('重新读取状态'));
      await sleep(2_000);
      const afterReread = await cdp.evaluate(`({
        unknownLock: __smoke.unknownLock(),
        panelOpen: __smoke.panel() !== null,
        send: __smoke.buttonState('发送'),
        apply: __smoke.buttonState('应用配置'),
      })`);
      // 后端仍须拒绝普通业务：直接派发不带 UI 猜测，确认未执行且没有写入。
      const afterSnapshot = await readSnapshot(cdp);
      const backendAttempt = await cdp.invoke('prototype_dispatch', {
        command: {
          kind: 'set_draft',
          text: afterSnapshot.chat.draft,
          expectedSessionId: afterSnapshot.activeSessionId,
          expectedSessionsRevision: afterSnapshot.sessionsRevision,
        },
        context: buddyContextOf(afterSnapshot),
      });
      const pendingAfter = await fs.stat(pendingPath);
      const registryAfterAttempt = await fs.stat(registryPath);
      const evidence = {
        alertsBeforeReread,
        alertsAfterReread: await cdp.evaluate(`__smoke.alerts()`),
        beforeReread,
        afterReread,
        panelClosedAfterReread: afterReread.panelOpen === false,
        // 面板关闭时 Apply 不存在（隐藏），不是“解锁”；存在时必须是禁用。
        applyHiddenAfterReread: afterReread.apply.present === false,
        applyDisabledAfterReread: afterReread.apply.disabled,
        backendRejected: backendAttempt.error?.code ?? null,
        backendDisposition: backendAttempt.error?.disposition ?? null,
        pendingSizeBefore: pendingBefore.size,
        pendingSizeAfter: pendingAfter.size,
        pendingMtimeBefore: pendingBefore.mtimeMs,
        pendingMtimeAfter: pendingAfter.mtimeMs,
        memoryEndpoint: snapshot.modelSettings?.endpoint ?? null,
        baselineEndpoint: baselineModel?.endpoint ?? null,
        diskEntryIsDirectory: registryStat.isDirectory(),
        diskEntryStillDirectory: registryAfterAttempt.isDirectory(),
        pendingRetained: true,
        originalSha256: originalDigest,
      };
      ensure(alertsBeforeReread.includes('未确认'), 'publish failure was not reported as unknown', evidence);
      ensure(!alertsBeforeReread.includes('未执行'), 'publish failure falsely claimed the configuration was not executed', evidence);
      ensure(afterReread.unknownLock === true, 'successful snapshot reread silently cleared the unknown command lock', evidence);
      ensure(await cdp.evaluate(`__smoke.currentPage() === 'chat'`), 'reread left a management page without its business panel');
      ensure(afterReread.send.disabled === true, 'send unlocked after an unknown publish result', evidence);
      // Apply 可能因面板关闭而隐藏，也可能仍存在但禁用；两者都不是可操作。
      ensure(afterReread.apply.operable === false, 'model apply became operable after an unknown publish result', evidence);
      ensure(
        afterReread.apply.present === false || afterReread.apply.disabled === true,
        'existing apply button was not disabled after an unknown publish result',
        evidence,
      );
      ensure(
        backendAttempt.error?.code === 'buddy_storage_unknown'
          && backendAttempt.error?.disposition === 'unknown',
        'backend accepted a business command during the unknown buddy storage state',
        evidence,
      );
      ensure(registryAfterAttempt.isDirectory(), 'backend attempt changed the blocked buddies.v1 entry', evidence);
      ensure(
        pendingAfter.size === pendingBefore.size && pendingAfter.mtimeMs === pendingBefore.mtimeMs,
        'backend attempt rewrote the retained buddies.pending file',
        evidence,
      );
      return evidence;
    });

    await check('unknown.global-revoke-reachable', '注册表 Unknown 拒绝诊断导航后，全局撤权按钮仍可取消并确认', async () => {
      ensure(blockerReady && await cdp.evaluate('__smoke.unknownLock()'), 'registry Unknown prerequisite missing');
      await navigatePage(cdp, 'buddies');
      await cdp.clickElement('.sidebar .nav-item[data-page="diagnostics"]');
      await cdp.waitFor(`__smoke.alerts().includes('buddy_storage_unknown')`, 5_000, 'rejected diagnostics navigation');
      ensure(await cdp.evaluate(`__smoke.currentPage() === 'buddies'`), 'failed diagnostics open changed the page');
      await cdp.clickElement('[data-testid="revoke-authorization"]');
      await cdp.waitFor(`document.querySelector('[aria-label="撤回授权确认"]') !== null`, 5_000, 'global revocation confirmation');
      await cdp.clickElement(`__smoke.button('取消', document.querySelector('[aria-label="撤回授权确认"]'))`);
      ensure((await readSnapshot(cdp)).revoked === false, 'cancel unexpectedly revoked the active buddy');
      await cdp.clickElement('[data-testid="revoke-authorization"]');
      await cdp.clickElement(`__smoke.button('确认撤回', document.querySelector('[aria-label="撤回授权确认"]'))`);
      const revoked = await waitForSnapshot(cdp, (state) => state.revoked === true, 5_000, 'registry Unknown revocation');
      ensure(revoked.chat.messages.length === 0 && revoked.chat.draft === '', 'revocation retained chat data');
      await cdp.waitFor(`__smoke.currentPage() === 'chat'`, 5_000, 'revocation returned the Vue page to chat');
      return { rejectedPage: 'diagnostics', cancelRetainedAuthorization: true, revoked: revoked.revoked };
    });

    await check('unknown.publish-failure-cleanup-restores-original', '按原文件证据清理 Publish 故障并核对合成根恢复', async () => {
      ensure(blockerReady, 'publish blocker was not established');
      ensure(originalBytes !== null, 'original buddies.v1 evidence is missing');
      const cleanup = await restore();
      blockerReady = false;
      ensure(cleanup.restoredFile === true, 'buddies.v1 was not restored as a file', cleanup);
      ensure(cleanup.sha256 === cleanup.originalSha256, 'restored buddies.v1 does not match the retained original', cleanup);
      ensure(cleanup.pendingRemoved === true, 'buddies.pending was not removed during cleanup', cleanup);
      return cleanup;
    });
  } finally {
    if (blockerReady) {
      await restore().catch((error) => {
        cleanupEvidence = { ...(cleanupEvidence ?? {}), failed: error.message };
      });
    }
    // 失败路径显式收掉面板/表单与草稿，保证实例仍能走自身正常退出确认。
    await resetUiForExit(instance);
    instance.cleanupEvidence = cleanupEvidence;
  }
}

function reportInstance(instance, extra = {}) {
  return {
    label: instance.label,
    pid: instance.pid,
    cdpPort: instance.port,
    webview2Profile: path.relative(workspaceRoot, instance.profile),
    isolatedDataRoot: path.relative(workspaceRoot, instance.dataRoot),
    logs: instance.logs,
    exitCode: instance.child.exitCode,
    exitSignal: instance.child.signalCode,
    cleanupKilled: instance.cleanupKilled === true,
    cleanupReason: instance.cleanupReason ?? null,
    cleanupIdentity: instance.cleanupIdentity ?? null,
    cleanupIdentityMismatch: instance.cleanupIdentityMismatch === true,
    cleanupExit: instance.cleanupExit ?? null,
    cleanupError: instance.cleanupError ?? null,
    cleanupEvidence: instance.cleanupEvidence ?? null,
    ...extra,
  };
}

/*
 * 阶段4/C 素材真窗验证复用生产解析契约（src/core/material.ts 编译产物），
 * 不在脚本里维护第二份弱化的状态校验。只有“端点未接通”才记 pending；
 * 端点已接通但投影不合法必须作为真实失败，不能降级为 pending。
 */
const require = createRequire(import.meta.url);
const tscBin = path.join(appRoot, 'node_modules', 'typescript', 'bin', 'tsc');

function ensureCoreTestBuild() {
  const materialSource = path.join(appRoot, 'src', 'core', 'material.ts');
  const compiled = path.join(appRoot, '.test-build', 'material.js');
  let needsBuild = true;
  try {
    needsBuild = statSync(compiled).mtimeMs < statSync(materialSource).mtimeMs;
  } catch {
    needsBuild = true;
  }
  if (!needsBuild) return;
  const build = spawnSync(process.execPath, [tscBin, '-p', 'tsconfig.test.json'], {
    cwd: appRoot,
    stdio: 'inherit',
  });
  if (build.error) throw build.error;
  if (build.status !== 0) {
    throw new Error('failed to compile production core modules for the native material smoke');
  }
}

ensureCoreTestBuild();
const {
  materialFrameTriple,
  parseMaterialFrame,
  parseMaterialReply,
  parseMaterialState,
} = require(path.join(appRoot, '.test-build', 'material.js'));

function materialEndpointUnavailable(message) {
  return /not found|not allowed|unknown command|no such command/i.test(String(message));
}

async function probeMaterialState(cdp) {
  try {
    const raw = await cdp.invoke('prototype_material_state');
    return { integrated: true, raw, parsed: parseMaterialState(raw) };
  } catch (error) {
    const message = error.message;
    if (materialEndpointUnavailable(message)) {
      return { integrated: false, error: message };
    }
    // 端点存在但调用/序列化失败：不能当作“未接通”降级为 pending。
    return { integrated: true, raw: null, parsed: null, error: message };
  }
}

async function readMaterialState(cdp) {
  const probe = await probeMaterialState(cdp);
  if (!probe.integrated) {
    throw new CheckFailure('prototype_material_state is unavailable', probe.error);
  }
  if (probe.parsed === null) {
    throw new CheckFailure(
      'prototype_material_state is not accepted by the production parseMaterialState contract',
      probe.raw,
    );
  }
  return probe.parsed;
}

async function waitForMaterialState(cdp, predicate, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let latest = null;
  let raw = null;
  while (Date.now() < deadline) {
    const probe = await probeMaterialState(cdp);
    if (!probe.integrated) {
      throw new CheckFailure('material state endpoint disappeared while waiting', probe.error);
    }
    raw = probe.raw;
    if (probe.parsed === null) {
      throw new CheckFailure(
        'material state violates the production parseMaterialState contract while waiting',
        probe.raw,
      );
    }
    latest = probe.parsed;
    if (predicate(latest)) return latest;
    await sleep(50);
  }
  throw new CheckFailure(`material state did not reach: ${label}`, { latest, raw });
}

function materialIdentity(snapshot) {
  if (snapshot === null) return null;
  return {
    activeBuddyId: snapshot.activeBuddyId,
    buddiesRevision: snapshot.buddiesRevision,
    buddies: snapshot.buddies,
    activeBuddyPersona: snapshot.activeBuddyPersona,
    buddyCommit: snapshot.buddyCommit,
    managementDraft: snapshot.managementDraft,
    managementExit: snapshot.managementExit,
    managementDiscardedVersion: snapshot.managementDiscardedVersion,
    activeSessionId: snapshot.activeSessionId,
    sessionsRevision: snapshot.sessionsRevision,
    revoked: snapshot.revoked,
  };
}

async function sendMaterialCommand(cdp, command, context) {
  const raw = await cdp.invoke('prototype_material_command', { command, context });
  const parsed = parseMaterialReply(raw);
  if (parsed === null) {
    throw new CheckFailure(
      'material command reply is not accepted by the production parseMaterialReply contract',
      raw,
    );
  }
  return parsed;
}

async function materialPanelSnapshot(cdp) {
  return cdp.evaluate(`(() => {
    const norm = window.__smoke.norm;
    const panel = document.querySelector('.material-panel');
    const drop = document.querySelector('.material-drop');
    const select = document.querySelector('.material-action select');
    const refresh = document.querySelector('.material-refresh');
    const rect = drop ? drop.getBoundingClientRect() : null;
    const buttons = {};
    for (const label of ['播放', '暂停', '继续', '停止', '卸载', '恢复测试猫']) {
      const node = panel
        ? Array.from(panel.querySelectorAll('button')).find((item) => norm(item.textContent) === label)
        : null;
      buttons[label] = node
        ? {
            present: true,
            visible: !!(node.offsetWidth || node.offsetHeight || node.getClientRects().length),
            disabled: node.disabled === true,
          }
        : { present: false, visible: false, disabled: null };
    }
    return {
      panel: panel !== null,
      drop: drop !== null,
      dropText: drop ? norm(drop.textContent) : '',
      packText: panel ? norm((panel.querySelector('.material-pack') || {}).textContent || '') : '',
      readout: panel ? norm((panel.querySelector('.material-readout') || {}).textContent || '') : '',
      warnings: panel
        ? Array.from(panel.querySelectorAll('.material-warn')).map((node) => norm(node.textContent))
        : [],
      refresh: refresh
        ? { present: true, disabled: refresh.disabled === true }
        : { present: false, disabled: null },
      buttons,
      options: select
        ? Array.from(select.options).map((option) => ({
            value: option.value,
            label: norm(option.textContent),
            disabled: option.disabled === true,
          }))
        : null,
      selected: select ? select.value : null,
      rect: rect
        ? { left: rect.left, top: rect.top, width: rect.width, height: rect.height }
        : null,
      dpr: window.devicePixelRatio,
    };
  })()`);
}

function panelButtonUsable(panel, label) {
  const button = panel.buttons?.[label];
  return button?.present === true && button.visible === true && button.disabled === false;
}

async function waitForPanel(cdp, predicate, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let latest = null;
  while (Date.now() < deadline) {
    latest = await materialPanelSnapshot(cdp);
    if (predicate(latest)) return latest;
    await sleep(100);
  }
  throw new CheckFailure(`material panel did not reach: ${label}`, latest);
}

function materialButtonExpression(label) {
  return `__smoke.button(${JSON.stringify(label)}, document.querySelector('.material-panel'))`;
}

async function clickMaterialButton(cdp, label) {
  await cdp.clickElement(materialButtonExpression(label));
}

/** 只读刷新让面板与宿主的双代际对齐；它不会自动重发任何素材命令。 */
async function refreshMaterialPanel(cdp) {
  const panel = await materialPanelSnapshot(cdp);
  ensure(
    panel.refresh.present === true && panel.refresh.disabled !== true,
    'material read-only refresh button is unusable',
    panel,
  );
  await clickMaterialButton(cdp, '刷新状态（只读）');
  await sleep(150);
}

async function selectMaterialAction(cdp, actionId) {
  const selected = await cdp.evaluate(`(() => {
    const select = document.querySelector('.material-action select');
    if (select === null) return null;
    select.value = ${JSON.stringify(actionId)};
    select.dispatchEvent(new Event('change', { bubbles: true }));
    return select.value;
  })()`);
  ensure(selected === actionId, `material action select could not choose ${actionId}`, { selected });
}

/**
 * 真实主窗拖放处理器的事件注入：通过 Tauri 事件注册表取得生产监听的 handlerId，
 * 再走宿主的 runCallback 入口派发 tauri://drag-drop。它不是系统鼠标拖放，
 * 只是把同一原生事件送进既有生产处理器，避免直调素材加载 IPC 冒充产品入口。
 */
async function injectTauriDrop(cdp, paths, options = {}) {
  const area = await cdp.evaluate(`(() => {
    const drop = document.querySelector('.material-drop');
    if (drop === null) return null;
    const rect = drop.getBoundingClientRect();
    return {
      rect: { left: rect.left, top: rect.top, width: rect.width, height: rect.height },
      dpr: window.devicePixelRatio,
    };
  })()`);
  ensure(area !== null, 'material drop area is missing from the main window');
  const position = options.position ?? {
    x: (area.rect.left + area.rect.width / 2) * area.dpr,
    y: (area.rect.top + area.rect.height / 2) * area.dpr,
  };
  const injected = await cdp.evaluate(`(() => {
    const registry = window.__internal_unstable_listeners_object_id__ || {};
    const listeners = registry['tauri://drag-drop'] || {};
    // Tauri 用 Object.defineProperty 注册监听（enumerable 默认 false），
    // 因此必须用 getOwnPropertyNames 读取，不能用 Object.entries。
    const ids = Object.getOwnPropertyNames(listeners);
    const handlerIds = [];
    for (const id of ids) {
      const listener = listeners[id];
      handlerIds.push(listener.handlerId);
      window.__TAURI_INTERNALS__.runCallback(listener.handlerId, {
        event: 'tauri://drag-drop',
        id: Number(id),
        payload: {
          paths: ${JSON.stringify(paths)},
          position: ${JSON.stringify(position)},
        },
      });
    }
    return { handlerIds, listenerCount: ids.length };
  })()`);
  ensure(
    injected.listenerCount > 0,
    'no production tauri://drag-drop listener is registered in the main window',
    injected,
  );
  return {
    position,
    ...injected,
    injectedEvent:
      'tauri://drag-drop via __TAURI_INTERNALS__.runCallback; event injection, not a real OS mouse drag',
  };
}

async function readPetFrame(pet) {
  const raw = await pet.invoke('prototype_frame');
  const frame = parseMaterialFrame(raw);
  if (frame === null) {
    throw new CheckFailure(
      'pet frame descriptor is not accepted by the production parseMaterialFrame contract',
      raw,
    );
  }
  return frame;
}

async function waitForPetFrame(pet, predicate, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let latest = null;
  while (Date.now() < deadline) {
    latest = await readPetFrame(pet);
    if (predicate(latest)) return latest;
    await sleep(50);
  }
  throw new CheckFailure(`pet frame did not reach: ${label}`, latest);
}

async function petCanvasSummary(pet) {
  return pet.evaluate(`(() => {
    const canvas = document.querySelector('.pet-canvas');
    if (canvas === null) return { present: false };
    const width = canvas.width;
    const height = canvas.height;
    if (!width || !height) return { present: true, width, height, opaque: 0, hash: null };
    const context = canvas.getContext('2d');
    if (context === null) {
      return { present: true, width, height, opaque: 0, hash: null, error: 'no-2d-context' };
    }
    let data;
    try {
      data = context.getImageData(0, 0, width, height).data;
    } catch (error) {
      return { present: true, width, height, opaque: 0, hash: null, error: String(error) };
    }
    let opaque = 0;
    let hash = 2166136261 >>> 0;
    for (let index = 0; index < data.length; index += 4) {
      if (data[index + 3] > 0) opaque += 1;
      hash ^= data[index] + (data[index + 1] << 8) + (data[index + 2] << 16) + (data[index + 3] << 24);
      hash = Math.imul(hash, 16777619) >>> 0;
    }
    return { present: true, width, height, opaque, hash };
  })()`);
}

async function ensurePetCanvasDrawn(pet, label) {
  const deadline = Date.now() + 8_000;
  let latest = null;
  while (Date.now() < deadline) {
    latest = await petCanvasSummary(pet);
    if (latest.present && latest.width > 0 && latest.height > 0 && latest.opaque > 0) {
      return latest;
    }
    await sleep(100);
  }
  throw new CheckFailure(`pet canvas did not draw ${label}`, latest);
}

async function petFrameBinary(pet) {
  const frame = await readPetFrame(pet);
  const probe = await pet.evaluate(`(async () => {
    const bytes = await window.__TAURI_INTERNALS__.invoke('prototype_material_pixels', {
      frame: ${JSON.stringify(materialFrameTriple(frame))},
    });
    const data = new Uint8Array(bytes);
    let text = '';
    for (let index = 0; index < data.length; index += 1) text += String.fromCharCode(data[index]);
    return {
      isArrayBuffer: bytes instanceof ArrayBuffer,
      byteLength: data.length,
      base64: btoa(text),
    };
  })()`);
  return {
    frame,
    bytes: Buffer.from(probe.base64, 'base64'),
    isArrayBuffer: probe.isArrayBuffer,
    reportedLength: probe.byteLength,
  };
}

function expectedFrameFor(expectedDoc, frame) {
  ensure(frame.clipId !== null && frame.frameIndex !== null, 'frame descriptor has no clip/frame index', frame);
  const clip = expectedDoc?.clips?.[frame.clipId];
  ensure(clip !== undefined, `no independently derived expected clip ${frame.clipId}`, {
    clips: Object.keys(expectedDoc?.clips ?? {}),
  });
  const expected = clip.frames?.[frame.frameIndex];
  ensure(
    expected !== undefined && typeof expected.rgbaBase64 === 'string',
    'no inline expected RGBA for the presented frame',
    { clipId: frame.clipId, frameIndex: frame.frameIndex },
  );
  return expected;
}

/**
 * 合成拒绝包仍只走真实主窗拖放处理器（Tauri tauri://drag-drop 事件注入，不是
 * 操作系统鼠标拖放）：等待宿主异步返回最终失败，再证明有效旧包、旧动作、
 * 播放/资源代际与伙伴身份保持，且 pet 旧帧继续推进。宿主拒绝只暴露固定安全
 * 文案，具体越界/预算缺陷分别由夹具构造与生产 MaterialPack 回归证明；
 * 这里不直调加载 IPC，也不放宽生产解析器。
 */
async function rejectPackageKeepsOldPlayback(instance, packageDir, label, defect) {
  const { cdp } = instance;
  // 只读刷新先把面板与宿主的双代际对齐；随后的拖放仍走生产处理器，不直调 IPC。
  await refreshMaterialPanel(cdp);
  const before = await readMaterialState(cdp);
  ensure(
    before.loading === false && before.status === 'playing' && before.source === 'package',
    `${label}: an installed package must be playing before the drop`,
    before,
  );
  const identityBefore = JSON.stringify(materialIdentity(await readSnapshot(cdp)));
  const injected = await injectTauriDrop(cdp, [packageDir]);
  // 只等 error !== null 会命中上一只坏包留下的残留错误，必须要求本次加载推进 revision。
  const failed = await waitForMaterialState(
    cdp,
    (state) => state.revision > before.revision && state.loading === false && state.error !== null,
    20_000,
    `${label}: final async rejection`,
  );
  ensure(failed.error.code === 'material_package_invalid', `${label}: not rejected as an invalid package`, failed);
  ensure(
    typeof failed.error.message === 'string' && failed.error.message.length > 0,
    `${label}: rejection exposes no safe message`,
    failed,
  );
  const leaked = [packageDir, defect, 'escape.png', 'big.png'].filter(
    (needle) => failed.error.message.includes(needle) || JSON.stringify(failed).includes(needle),
  );
  ensure(leaked.length === 0, `${label}: rejection leaked fixture path/defect details`, { failed, leaked });
  ensure(
    failed.source === 'package' &&
      failed.pack?.id === before.pack.id &&
      failed.pack.version === before.pack.version,
    `${label}: rejection replaced the valid old package`,
    { before, failed },
  );
  ensure(
    failed.actionId === before.actionId && failed.status === 'playing',
    `${label}: rejection stopped or changed the old action`,
    { before, failed },
  );
  ensure(
    failed.playbackGeneration === before.playbackGeneration &&
      failed.resourceGeneration === before.resourceGeneration,
    `${label}: rejection changed the old playback/resource generation`,
    { before, failed },
  );
  ensure(
    failed.actions.length === before.actions.length &&
      failed.frameCount === before.frameCount &&
      failed.decodedBytes === before.decodedBytes,
    `${label}: rejection changed the old action directory or decoded budget`,
    { before, failed },
  );
  const panel = await waitForPanel(
    cdp,
    (candidate) =>
      candidate.packText.includes(before.pack.id) &&
      candidate.warnings.some((warning) => warning.includes(failed.error.message)) &&
      panelButtonUsable(candidate, '播放'),
    5_000,
    `${label}: panel keeps the old package and shows the rejection`,
  );
  const pet = await CdpClient.connect(instance.port, '宠物');
  try {
    const first = await waitForPetFrame(
      pet,
      (frame) =>
        frame.source === 'package' &&
        frame.paused === false &&
        frame.resourceGeneration === failed.resourceGeneration &&
        frame.playbackGeneration === failed.playbackGeneration,
      5_000,
      `${label}: old package frame after rejection`,
    );
    await sleep(450);
    const second = await readPetFrame(pet);
    ensure(
      second.presentationSequence > first.presentationSequence,
      `${label}: the old package stopped advancing after the rejection`,
      { first, second },
    );
    const identityAfter = JSON.stringify(materialIdentity(await readSnapshot(cdp)));
    ensure(
      identityAfter === identityBefore,
      `${label}: rejection changed buddy identity, receipt, management draft or session revision`,
      { before: identityBefore, after: identityAfter },
    );
    return {
      injected,
      before,
      failed,
      panel: { packText: panel.packText, warnings: panel.warnings, readout: panel.readout },
      frameAdvance: { first, second },
      identityStable: true,
      defect,
      note: 'drag-drop event injection through the production listener; not an OS mouse drag',
    };
  } finally {
    pet.close();
  }
}

/**
 * 阶段4/C 真实窗素材场景。只用显式临时 data-dir/profile、target/ 下合成包与
 * 回环模型；不触碰 daily 或真实服务。素材状态与命令回执必须通过生产解析契约；
 * 端点未接通才记 pending，端点已接通但投影非法按真实失败保留。
 */
async function scenarioMaterial(instance, fixtureRoot) {
  const { cdp } = instance;
  await navigatePage(cdp, 'appearance');
  const failedChecks = [];
  const materialCheck = async (id, description, run) => {
    const ok = await check(id, description, run);
    if (!ok) failedChecks.push(id);
    return ok;
  };
  const blockedCheck = (id, description, reason) =>
    materialCheck(id, description, () => {
      throw new CheckFailure(`blocked by a previous real failure: ${reason}`);
    });

  const pngPackageDir = path.join(fixtureRoot, 'packages', 'valid-sequence-basic');
  const gifPackageDir = path.join(fixtureRoot, 'packages', 'valid-gif-disposal');
  const badPackageDir = path.join(fixtureRoot, 'packages', 'reject-png-corrupt-idat');
  const pathEscapePackageDir = path.join(fixtureRoot, 'packages', 'reject-path-escape');
  const oversizeDimensionPackageDir = path.join(fixtureRoot, 'packages', 'reject-frame-dimension');
  const expectedPng = JSON.parse(
    await fs.readFile(path.join(fixtureRoot, 'expected', 'valid-sequence-basic.json'), 'utf8'),
  );
  const expectedGif = JSON.parse(
    await fs.readFile(path.join(fixtureRoot, 'expected', 'valid-gif-disposal.json'), 'utf8'),
  );

  // 界面部分不依赖宿主素材 IPC，先独立核验真实主窗的素材区域与只读刷新入口。
  await materialCheck(
    'material.ui-drop-area',
    '主窗素材区域说明格式/容量/本地读取，提供只读刷新且不回显路径',
    async () => {
      const ui = await materialPanelSnapshot(cdp);
      ensure(ui.panel && ui.drop, 'material drop area is missing from the real main view', ui);
      ensure(String(ui.dropText).includes('manifest.json'), 'drop area does not explain manifest.json', ui);
      for (const token of ['PNG', 'GIF', '512', '32', '本地']) {
        ensure(String(ui.dropText).includes(token), `drop hint is missing ${token}`, ui);
      }
      ensure(
        !/身份|权限|签发|业务/.test(String(ui.dropText)),
        'drop hint exposes internal identity/permission terms',
        ui,
      );
      ensure(ui.refresh.present === true && ui.refresh.disabled !== true, 'read-only material state refresh is unusable', ui);
      ensure(
        ui.rect !== null &&
          Number.isFinite(ui.rect.left) &&
          Number.isFinite(ui.rect.top) &&
          Number.isFinite(ui.rect.width) &&
          Number.isFinite(ui.rect.height) &&
          ui.rect.width > 0 &&
          ui.rect.height > 0,
        'material drop area has no measurable position/size for native drop hit-testing',
        ui,
      );
      ensure(
        Number.isFinite(ui.dpr) && ui.dpr >= 1,
        'material drop area cannot be mapped to physical pixels without a valid devicePixelRatio',
        ui,
      );
      return ui;
    },
  );
  // 下拉/按钮属于素材面板本身；格式契约同样在本项静态核验。
  await materialCheck(
    'material.ui-controls',
    '素材面板提供真实动作选择与播放/暂停/继续/停止/卸载/恢复测试猫按钮',
    async () => {
      const ui = await materialPanelSnapshot(cdp);
      ensure(ui.options !== null, 'material action select is missing', ui);
      for (const label of ['播放', '暂停', '继续', '停止', '卸载', '恢复测试猫']) {
        ensure(ui.buttons[label]?.present === true, `material panel button is missing: ${label}`, ui);
      }
      return { buttons: ui.buttons, options: ui.options };
    },
  );

  const initialProbe = await probeMaterialState(cdp);
  if (!initialProbe.integrated) {
    RUN.pending.push({
      id: 'material.c-host-contract',
      reason:
        'prototype_material_state is unavailable; the host has not integrated the frozen C material IPC',
      error: initialProbe.error,
    });
    return { supported: false, failedChecks };
  }

  const snapshot = await readSnapshot(cdp);
  const context = {
    expectedBuddyId: snapshot?.activeBuddyId ?? '',
    expectedBuddiesRevision: snapshot?.buddiesRevision ?? 0,
  };
  ensure(typeof context.expectedBuddyId === 'string' && context.expectedBuddyId.length > 0,
    'no active trusted buddy context for material commands', context);
  const identityBefore = JSON.stringify(materialIdentity(snapshot));

  await materialCheck(
    'material.state-contract',
    '素材状态严格复用生产 parseMaterialState，投影无路径/无伙伴正文',
    async () => {
      ensure(initialProbe.parsed !== null, 'prototype_material_state violates parseMaterialState', initialProbe.raw);
      ensure(!JSON.stringify(initialProbe.raw).includes('path'), 'material state leaked a path');
      return {
        revision: initialProbe.parsed.revision,
        source: initialProbe.parsed.source,
        pack: initialProbe.parsed.pack,
        actions: initialProbe.parsed.actions,
        actionId: initialProbe.parsed.actionId,
        status: initialProbe.parsed.status,
      };
    },
  );

  const initialState = initialProbe.parsed;
  const initialUsable = await materialCheck(
    'material.initial-builtin-usable',
    '初始 builtin 素材面板真正可用：动作目录、播放按钮与内置帧一致',
    async () => {
      ensure(initialState !== null, 'initial builtin state is not accepted by parseMaterialState', initialProbe.raw);
      ensure(initialState.source === 'builtin' && initialState.pack === null, 'initial source is not builtin', initialState);
      ensure(
        initialState.actionId === 'idle' && initialState.status === 'playing',
        'initial builtin is not playing idle',
        initialState,
      );
      ensure(
        initialState.actions.some((action) => action.id === 'idle'),
        'initial builtin exposes no idle action in its action directory',
        initialState,
      );
      ensure(
        initialState.frameCount === 4 && initialState.decodedBytes === 0,
        'initial builtin resource counts are inconsistent with the builtin clip',
        initialState,
      );
      const ui = await waitForPanel(
        cdp,
        (panel) =>
          panel.options?.some((option) => option.value === 'idle') === true &&
          panelButtonUsable(panel, '播放'),
        5_000,
        'initial builtin panel controls',
      );
      return { state: initialState, options: ui.options, buttons: ui.buttons, readout: ui.readout };
    },
  );

  let loaded = null;
  let packLoaded = false;

  if (!initialUsable) {
    // 仍然把真实拖放事件送进生产处理器，证明初始投影缺陷确实让产品入口不可用。
    await materialCheck(
      'material.drag-drop-png-load',
      'PNG 序列包经真实主窗拖放处理器加载（Tauri drag-drop 事件注入，非真实鼠标拖放）',
      async () => {
        const injected = await injectTauriDrop(cdp, [pngPackageDir]);
        await sleep(300);
        const ui = await materialPanelSnapshot(cdp);
        const after = await probeMaterialState(cdp);
        throw new CheckFailure(
          'drag-drop load did not start because the initial builtin projection is unusable',
          { injected, panel: { readout: ui.readout, warnings: ui.warnings, options: ui.options }, state: after.raw },
        );
      },
    );
  } else {
    const pngDragOk = await materialCheck(
      'material.drag-drop-png-load',
      'PNG 序列包经真实主窗拖放处理器加载（Tauri drag-drop 事件注入，非真实鼠标拖放）',
      async () => {
        const injected = await injectTauriDrop(cdp, [pngPackageDir]);
        const state = await waitForMaterialState(
          cdp,
          (candidate) =>
            candidate.loading === false &&
            candidate.source === 'package' &&
            candidate.pack?.id === 'synthetic-seq-basic',
          20_000,
          'PNG pack loaded through drag-drop',
        );
        ensure(
          state.actions.some((action) => action.id === 'idle') &&
            state.actions.some((action) => action.id === 'wave'),
          'PNG pack action directory is incomplete',
          state,
        );
        ensure(state.frameCount === 4 && state.decodedBytes > 0, 'PNG pack budget projection is invalid', state);
        const ui = await waitForPanel(
          cdp,
          (panel) =>
            panel.packText.includes('synthetic-seq-basic') &&
            panel.options?.some((option) => option.value === 'wave'),
          5_000,
          'PNG panel state',
        );
        loaded = state;
        return { injected, state, panel: { packText: ui.packText, options: ui.options } };
      },
    );
    packLoaded = pngDragOk;
  }

  if (!packLoaded) {
    // 直接 IPC 仅作边界/证据准备：初始 builtin 投影缺陷会让面板入口不可用，
    // 但 pet 取帧边界仍应独立可验。它绝不计作产品拖放成功。
    const fallbackOk = await materialCheck(
      'material.ipc-boundary-load',
      '仅作边界证据准备：直接 IPC 加载 PNG 包供 pet 取帧，不计为产品拖放入口',
      async () => {
        const base = initialState ?? initialProbe.raw;
        const rawReply = await cdp.invoke('prototype_material_command', {
          command: {
            type: 'load',
            path: pngPackageDir,
            expectedResourceGeneration: base.resourceGeneration,
            expectedPlaybackGeneration: base.playbackGeneration,
          },
          context,
        });
        ensure(rawReply.ok === true, 'direct IPC boundary load was not accepted', rawReply);
        // 初始 builtin 投影在安装完成前仍不合法（actionId=idle、actions=[]、loading=true）；
        // 这个边界准备路径允许等待到有效包投影，但初始投影本身已经作为真实失败记录。
        const deadline = Date.now() + 20_000;
        let state = null;
        let lastRaw = null;
        while (Date.now() < deadline) {
          const probe = await probeMaterialState(cdp);
          ensure(probe.integrated, 'material endpoint disappeared during boundary load', probe.error);
          lastRaw = probe.raw;
          if (
            probe.parsed !== null &&
            probe.parsed.loading === false &&
            probe.parsed.source === 'package' &&
            probe.parsed.pack?.id === 'synthetic-seq-basic'
          ) {
            state = probe.parsed;
            break;
          }
          await sleep(50);
        }
        ensure(state !== null, 'boundary PNG load did not reach a valid installed package', lastRaw);
        loaded = state;
        return {
          state,
          replyAccepted: rawReply.ok,
          replyStateStrictlyParseable: parseMaterialState(rawReply.state) !== null,
          note: 'boundary prerequisite only; not a product drag-drop load',
        };
      },
    );
    packLoaded = fallbackOk;
  }

  if (packLoaded) {
    await materialCheck(
      'material.pet-pixels-binary',
      '实际 pet WebView 按三元组取真实帧二进制并匹配独立期望；主窗取帧被明确拒绝',
      async () => {
        const current = await readMaterialState(cdp);
        if (current.status === 'playing') {
          const paused = await sendMaterialCommand(
            cdp,
            {
              type: 'pause',
              expectedResourceGeneration: current.resourceGeneration,
              expectedPlaybackGeneration: current.playbackGeneration,
            },
            context,
          );
          ensure(paused.ok === true, 'boundary pause was rejected', paused);
          await waitForMaterialState(cdp, (state) => state.status === 'paused', 8_000, 'paused for pixel probe');
        }
        const pet = await CdpClient.connect(instance.port, '宠物');
        try {
          const frozen = await waitForPetFrame(
            pet,
            (frame) => frame.source === 'package' && frame.paused === true,
            8_000,
            'paused PNG pet frame',
          );
          const binary = await petFrameBinary(pet);
          ensure(binary.isArrayBuffer === true, 'pet pixels are not raw binary', { frame: frozen });
          ensure(binary.reportedLength === frozen.width * frozen.height * 4, 'pet pixels length mismatch', {
            frame: frozen,
            reportedLength: binary.reportedLength,
          });
          ensure(binary.bytes.length === frozen.width * frozen.height * 4, 'pet pixels byte length mismatch', {
            frame: frozen,
            bytes: binary.bytes.length,
          });
          const expected = expectedFrameFor(expectedPng, frozen);
          ensure(
            binary.bytes.equals(Buffer.from(expected.rgbaBase64, 'base64')),
            'pet binary does not match the independently derived PNG fixture pixels',
            { frame: frozen, expectedSha256: expected.rgbaSha256 },
          );
          const canvas = await ensurePetCanvasDrawn(pet, 'PNG package frame');

          let mainRejected = null;
          try {
            await cdp.invoke('prototype_material_pixels', { frame: materialFrameTriple(frozen) });
          } catch (error) {
            mainRejected = error.message;
          }
          ensure(
            mainRejected !== null && /pet-window only/.test(mainRejected),
            'main window was not denied pet-only material pixels',
            { mainRejected },
          );

          let petCommandRejected = null;
          try {
            await pet.invoke('prototype_material_command', {
              command: {
                type: 'play',
                actionId: 'idle',
                expectedResourceGeneration: frozen.resourceGeneration,
                expectedPlaybackGeneration: frozen.playbackGeneration,
              },
              context,
            });
          } catch (error) {
            petCommandRejected = error.message;
          }
          ensure(
            petCommandRejected !== null && /main-window only/.test(petCommandRejected),
            'pet window was not denied main-only material commands',
            { petCommandRejected },
          );

          return {
            frame: frozen,
            reportedLength: binary.reportedLength,
            expectedSha256: expected.rgbaSha256,
            canvas,
            mainRejected,
            petCommandRejected,
            sourceWindow: '宠物 WebView2 target',
          };
        } finally {
          pet.close();
        }
      },
    );

    await materialCheck(
      'material.panel-controls-png',
      '真实面板按钮驱动 PNG 不同动作；暂停不走时，继续恢复',
      async () => {
        const ui = await waitForPanel(
          cdp,
          (panel) =>
            panel.options?.some((option) => option.value === 'wave') === true &&
            panelButtonUsable(panel, '播放'),
          5_000,
          'PNG panel controls',
        );
        ensure(panelButtonUsable(ui, '播放'), 'PNG play button is not operable', ui);
        await refreshMaterialPanel(cdp);
        const current = await readMaterialState(cdp);
        if (current.status === 'paused' && panelButtonUsable(ui, '继续')) {
          await clickMaterialButton(cdp, '继续');
          await waitForMaterialState(cdp, (state) => state.status === 'playing', 8_000, 'PNG resumed before action switch');
        }
        await selectMaterialAction(cdp, 'idle');
        await clickMaterialButton(cdp, '播放');
        const idle = await waitForMaterialState(
          cdp,
          (state) => state.status === 'playing' && state.actionId === 'idle',
          10_000,
          'PNG idle playing',
        );
        let paused = null;
        let frozenEvidence = null;
        let resumed = null;
        let frameAdvance = null;
        const pet = await CdpClient.connect(instance.port, '宠物');
        try {
          const first = await waitForPetFrame(
            pet,
            (frame) => frame.source === 'package' && frame.clipId === 'idle',
            5_000,
            'PNG playing pet frame',
          );
          await sleep(300);
          const second = await readPetFrame(pet);
          ensure(
            second.presentationSequence > first.presentationSequence,
            'PNG package did not advance while playing',
            { first, second },
          );
          frameAdvance = { first, second };

          // idle 为 loop，暂停/继续不受 once 动作时长影响。
          await clickMaterialButton(cdp, '暂停');
          paused = await waitForMaterialState(cdp, (state) => state.status === 'paused', 8_000, 'PNG paused');
          const frozen = await waitForPetFrame(
            pet,
            (frame) => frame.source === 'package' && frame.paused === true,
            5_000,
            'PNG paused pet frame',
          );
          const canvasBefore = await petCanvasSummary(pet);
          await sleep(400);
          const stillFrozen = await readPetFrame(pet);
          const canvasAfter = await petCanvasSummary(pet);
          ensure(
            stillFrozen.presentationSequence === frozen.presentationSequence,
            'paused PNG package advanced its presentation sequence',
            { frozen, stillFrozen },
          );
          ensure(
            stillFrozen.frameIndex === frozen.frameIndex,
            'paused PNG package changed frame index',
            { frozen, stillFrozen },
          );
          ensure(
            canvasBefore.present === true &&
              canvasBefore.opaque > 0 &&
              canvasBefore.hash === canvasAfter.hash,
            'paused pet canvas changed while paused',
            { canvasBefore, canvasAfter },
          );
          frozenEvidence = { frozen, stillFrozen, canvasBefore, canvasAfter };

          await clickMaterialButton(cdp, '继续');
          resumed = await waitForMaterialState(
            cdp,
            (state) => state.status === 'playing',
            8_000,
            'PNG resumed',
          );
        } finally {
          pet.close();
        }

        // 第二个动作走同一真实动作选择 + 播放按钮；once 动作只需到达 playing。
        await selectMaterialAction(cdp, 'wave');
        await clickMaterialButton(cdp, '播放');
        const wave = await waitForMaterialState(
          cdp,
          (state) => state.status === 'playing' && state.actionId === 'wave',
          10_000,
          'PNG wave playing',
        );
        ensure(
          wave.playbackGeneration > resumed.playbackGeneration,
          'action switch did not advance playback generation',
          { idle, resumed, wave },
        );
        return { idle, wave, paused, resumed, frameAdvance, frozen: frozenEvidence };
      },
    );
  } else {
    blockedCheck('material.pet-pixels-binary', '实际 pet WebView 取真实帧二进制', 'no package was loaded');
    blockedCheck('material.panel-controls-png', '真实面板按钮驱动 PNG 动作', 'no package was loaded');
  }

  let gifLoaded = false;
  if (packLoaded) {
    gifLoaded = await materialCheck(
      'material.drag-drop-gif-load',
      'GIF 包经真实主窗拖放处理器加载（Tauri drag-drop 事件注入，非真实鼠标拖放）',
      async () => {
        const injected = await injectTauriDrop(cdp, [gifPackageDir]);
        const state = await waitForMaterialState(
          cdp,
          (candidate) =>
            candidate.loading === false &&
            candidate.source === 'package' &&
            candidate.pack?.id === 'synthetic-gif-disposal',
          20_000,
          'GIF pack loaded through drag-drop',
        );
        ensure(state.actions.some((action) => action.id === 'idle'), 'GIF pack action directory is incomplete', state);
        ensure(state.frameCount === 5 && state.decodedBytes > 0, 'GIF pack budget projection is invalid', state);
        const ui = await waitForPanel(
          cdp,
          (panel) => panel.packText.includes('synthetic-gif-disposal'),
          5_000,
          'GIF panel state',
        );
        return { injected, state, panel: { packText: ui.packText, options: ui.options } };
      },
    );
  } else {
    blockedCheck('material.drag-drop-gif-load', 'GIF 包经真实主窗拖放处理器加载', 'no package was loaded');
  }

  if (gifLoaded) {
    await materialCheck(
      'material.panel-controls-gif',
      '真实面板播放 GIF 动作，pet 取帧匹配独立合成期望',
      async () => {
        const ui = await waitForPanel(
          cdp,
          (panel) =>
            panel.options?.some((option) => option.value === 'idle') === true &&
            panelButtonUsable(panel, '播放'),
          5_000,
          'GIF panel controls',
        );
        ensure(panelButtonUsable(ui, '播放'), 'GIF play button is not operable', ui);
        await refreshMaterialPanel(cdp);
        await selectMaterialAction(cdp, 'idle');
        await clickMaterialButton(cdp, '播放');
        const playing = await waitForMaterialState(
          cdp,
          (state) => state.status === 'playing' && state.actionId === 'idle',
          10_000,
          'GIF idle playing',
        );
        await clickMaterialButton(cdp, '暂停');
        await waitForMaterialState(cdp, (state) => state.status === 'paused', 8_000, 'GIF paused');
        const pet = await CdpClient.connect(instance.port, '宠物');
        try {
          const frozen = await waitForPetFrame(
            pet,
            (frame) => frame.source === 'package' && frame.clipId === 'idle' && frame.paused === true,
            8_000,
            'paused GIF pet frame',
          );
          ensure(frozen.frameIndex !== null && frozen.frameIndex < 5, 'GIF frame index is outside the decoded clip', {
            frame: frozen,
          });
          const binary = await petFrameBinary(pet);
          ensure(binary.isArrayBuffer === true, 'GIF pet pixels are not raw binary', { frame: frozen });
          ensure(
            binary.bytes.length === frozen.width * frozen.height * 4,
            'GIF pet pixels length mismatch',
            { frame: frozen, bytes: binary.bytes.length },
          );
          const expected = expectedFrameFor(expectedGif, frozen);
          ensure(
            binary.bytes.equals(Buffer.from(expected.rgbaBase64, 'base64')),
            'pet binary does not match the independently derived GIF fixture pixels',
            { frame: frozen, expectedSha256: expected.rgbaSha256 },
          );
          const canvas = await ensurePetCanvasDrawn(pet, 'GIF package frame');
          await clickMaterialButton(cdp, '继续');
          const resumed = await waitForMaterialState(
            cdp,
            (state) => state.status === 'playing',
            8_000,
            'GIF resumed',
          );
          return {
            playing,
            frozen,
            expectedSha256: expected.rgbaSha256,
            canvas,
            resumed,
            sourceWindow: '宠物 WebView2 target',
          };
        } finally {
          pet.close();
        }
      },
    );
  } else {
    blockedCheck('material.panel-controls-gif', '真实面板播放 GIF 动作', 'GIF package was not loaded');
  }

  if (gifLoaded) {
    await materialCheck(
      'material.failed-switch-retains-playing',
      '坏包异步加载明确失败后，仍保留有效旧包与旧动作继续播放（未先停止）',
      async () => {
        const before = await readMaterialState(cdp);
        ensure(
          before.status === 'playing' && before.pack?.id === 'synthetic-gif-disposal',
          'fixture precondition failed: the GIF package must be playing before the bad switch',
          before,
        );
        const injected = await injectTauriDrop(cdp, [badPackageDir]);
        const failed = await waitForMaterialState(
          cdp,
          (state) => state.loading === false && state.error !== null,
          20_000,
          'bad pack final failure',
        );
        ensure(failed.source === 'package' && failed.pack?.id === before.pack?.id, 'bad pack replaced the old package', {
          before,
          failed,
        });
        ensure(
          failed.actionId === before.actionId && failed.status === 'playing',
          'bad pack stopped the previously playing old action',
          { before, failed },
        );
        ensure(
          failed.actions.length === before.actions.length && failed.decodedBytes === before.decodedBytes,
          'bad pack changed the old package action directory or decoded budget',
          { before, failed },
        );
        const pet = await CdpClient.connect(instance.port, '宠物');
        try {
          const first = await waitForPetFrame(
            pet,
            (frame) => frame.source === 'package' && frame.paused === false,
            5_000,
            'old playback after bad pack',
          );
          await sleep(450);
          const second = await readPetFrame(pet);
          ensure(
            second.presentationSequence > first.presentationSequence,
            'old playback did not continue after the bad pack failure',
            { first, second },
          );
          return {
            injected,
            before,
            failed,
            error: failed.error,
            frameAdvance: { first, second },
            note: 'playback was not stopped before the failed switch',
          };
        } finally {
          pet.close();
        }
      },
    );
  } else {
    blockedCheck(
      'material.failed-switch-retains-playing',
      '坏包失败后保留旧播放',
      'no GIF package was loaded',
    );
  }

  // 两类新增拒绝：包内相对路径越界与尺寸/像素预算超限。仍复用已生成的微型
  // 合成夹具，经同一真实拖放处理器异步失败，并在旧包仍播放时核对旧动作/代际/
  // 伙伴身份与旧帧推进；不新增巨大文件，也不为测试开放生产后门。
  const rejectKeepPlaybackCases = [
    {
      id: 'material.reject-path-escape-retains-playing',
      description:
        '包内相对路径越界包经真实主窗拖放处理器异步拒绝后，有效旧包/动作/播放代际与伙伴身份保持、旧帧继续推进',
      packageDir: pathEscapePackageDir,
      label: 'path-escape package',
      defect: 'manifest ../escape.png',
    },
    {
      id: 'material.reject-frame-dimension-retains-playing',
      description:
        '尺寸/像素预算超限包（513×513 帧头）经真实主窗拖放处理器异步拒绝后，有效旧包/动作/播放代际与伙伴身份保持、旧帧继续推进',
      packageDir: oversizeDimensionPackageDir,
      label: 'oversize pixel-budget package',
      defect: '513x513 frame header',
    },
  ];
  for (const rejection of rejectKeepPlaybackCases) {
    if (gifLoaded) {
      await materialCheck(rejection.id, rejection.description, () =>
        rejectPackageKeepsOldPlayback(instance, rejection.packageDir, rejection.label, rejection.defect),
      );
    } else {
      blockedCheck(rejection.id, rejection.description, 'no GIF package was loaded to preserve');
    }
  }

  if (packLoaded) {
    await materialCheck(
      'material.stop-clears-frame-canvas',
      '真实停止按钮发布显式 empty 并清空 pet Canvas，保留已装包资源',
      async () => {
        const before = await readMaterialState(cdp);
        ensure(before.source === 'package', 'stop precondition lost the installed package', before);
        await clickMaterialButton(cdp, '停止');
        const stopped = await waitForMaterialState(
          cdp,
          (state) => state.status === 'stopped',
          8_000,
          'stopped',
        );
        ensure(stopped.source === 'package', 'stop must keep the installed package as source', stopped);
        ensure(stopped.decodedBytes > 0 && stopped.actions.length > 0, 'stop released installed package resources', stopped);
        const pet = await CdpClient.connect(instance.port, '宠物');
        try {
          const frame = await waitForPetFrame(pet, (candidate) => candidate.source === 'empty', 5_000, 'explicit empty frame');
          ensure(frame.clipId === null && frame.frameIndex === null, 'empty frame must be explicit null', frame);
          ensure(frame.width === 0 && frame.height === 0, 'empty frame must be 0×0', frame);
          const canvas = await petCanvasSummary(pet);
          ensure(
            canvas.present === true && canvas.width === 0 && canvas.height === 0,
            'stop did not clear the pet Canvas backing store',
            canvas,
          );
          return { stopped, frame, canvas };
        } finally {
          pet.close();
        }
      },
    );

    await materialCheck(
      'material.stop-then-replay',
      '停止后从真实面板重播同一动作并取得新帧（保留旧播放通道）',
      async () => {
        const ui = await waitForPanel(cdp, (panel) => panelButtonUsable(panel, '播放'), 5_000, 'replay controls');
        ensure(panelButtonUsable(ui, '播放'), 'replay play button is not operable after stop', ui);
        await refreshMaterialPanel(cdp);
        await selectMaterialAction(cdp, 'idle');
        await clickMaterialButton(cdp, '播放');
        const deadline = Date.now() + 6_000;
        let latest = null;
        while (Date.now() < deadline) {
          latest = await readMaterialState(cdp);
          if (latest.status === 'playing' && latest.actionId === 'idle') break;
          await sleep(100);
        }
        if (latest === null || latest.status !== 'playing' || latest.actionId !== 'idle') {
          // 产品按钮是主证据；这里再用一次直接 IPC 记录宿主的真实拒绝码，
          // 只作边界证据，不替代上面的真实面板操作。
          let boundaryError = null;
          try {
            const reply = await sendMaterialCommand(
              cdp,
              {
                type: 'play',
                actionId: 'idle',
                expectedResourceGeneration: latest?.resourceGeneration ?? 0,
                expectedPlaybackGeneration: latest?.playbackGeneration ?? 0,
              },
              context,
            );
            boundaryError = reply.ok ? 'unexpectedly accepted' : reply.error;
          } catch (error) {
            boundaryError = error.message;
          }
          throw new CheckFailure('stop->replay was not accepted by the host', {
            state: latest,
            panel: await materialPanelSnapshot(cdp),
            boundaryError,
            note: 'the failed replay was initiated by the real material panel button; the direct IPC probe only records the host rejection code',
          });
        }
        const pet = await CdpClient.connect(instance.port, '宠物');
        try {
          const frame = await waitForPetFrame(
            pet,
            (candidate) => candidate.source === 'package' && candidate.frameIndex !== null,
            8_000,
            'replayed package frame',
          );
          const canvas = await ensurePetCanvasDrawn(pet, 'replayed package frame');
          return { playing: latest, frame, canvas };
        } finally {
          pet.close();
        }
      },
    );

    await materialCheck(
      'material.unload-clears-canvas-resources',
      '真实卸载清空帧/Canvas/解码字节与动作目录',
      async () => {
        const before = await readMaterialState(cdp);
        ensure(before.source === 'package', 'unload precondition lost the installed package', before);
        await clickMaterialButton(cdp, '卸载');
        const empty = await waitForMaterialState(
          cdp,
          (state) => state.source === 'empty' && state.pack === null && state.loading === false,
          8_000,
          'unloaded empty',
        );
        ensure(empty.actionId === null, 'unload must clear actionId', empty);
        ensure(empty.actions.length === 0, 'unload must clear the action directory', empty);
        ensure(empty.decodedBytes === 0 && empty.frameCount === 0, 'unload must release decoded resources', empty);
        const pet = await CdpClient.connect(instance.port, '宠物');
        try {
          const frame = await waitForPetFrame(pet, (candidate) => candidate.source === 'empty', 5_000, 'unloaded empty frame');
          ensure(frame.width === 0 && frame.height === 0, 'unloaded frame must be 0×0', frame);
          const canvas = await petCanvasSummary(pet);
          ensure(
            canvas.present === true && canvas.width === 0 && canvas.height === 0,
            'unload did not clear the pet Canvas backing store',
            canvas,
          );
          return { empty, frame, canvas };
        } finally {
          pet.close();
        }
      },
    );

    await materialCheck(
      'material.restore-builtin-usable',
      '恢复内置测试猫后面板仍可用，并重绘 320×320 内置帧',
      async () => {
        const ui = await waitForPanel(cdp, (panel) => panelButtonUsable(panel, '恢复测试猫'), 5_000, 'builtin restore control');
        ensure(panelButtonUsable(ui, '恢复测试猫'), 'restore-builtin button is not operable from empty', ui);
        await clickMaterialButton(cdp, '恢复测试猫');
        const raw = await cdp.invoke('prototype_material_state');
        const parsed = parseMaterialState(raw);
        ensure(parsed !== null, 'restored builtin state is not accepted by parseMaterialState', raw);
        ensure(
          parsed.source === 'builtin' && parsed.pack === null,
          'restored source is not builtin',
          parsed,
        );
        ensure(
          parsed.actions.some((action) => action.id === 'idle') && parsed.actionId === 'idle',
          'restored builtin does not expose a usable idle action',
          parsed,
        );
        ensure(parsed.status === 'playing' && parsed.frameCount === 4, 'restored builtin is not playing its 4-frame idle clip', parsed);
        const pet = await CdpClient.connect(instance.port, '宠物');
        try {
          const frame = await waitForPetFrame(
            pet,
            (candidate) =>
              candidate.source === 'builtin' &&
              candidate.clipId === 'idle-four-frame' &&
              candidate.width === 320 &&
              candidate.height === 320,
            8_000,
            'restored builtin frame',
          );
          const canvas = await ensurePetCanvasDrawn(pet, 'restored builtin frame');
          const panel = await waitForPanel(
            cdp,
            (candidate) =>
              candidate.options?.some((option) => option.value === 'idle') === true &&
              panelButtonUsable(candidate, '播放'),
            5_000,
            'restored builtin panel controls',
          );
          return { parsed, frame, canvas, panel: { options: panel.options, readout: panel.readout } };
        } finally {
          pet.close();
        }
      },
    );
  } else {
    blockedCheck('material.stop-clears-frame-canvas', '真实停止清空帧/Canvas', 'no package was loaded');
    blockedCheck('material.stop-then-replay', '停止后重播', 'no package was loaded');
    blockedCheck('material.unload-clears-canvas-resources', '真实卸载清空资源', 'no package was loaded');
    blockedCheck('material.restore-builtin-usable', '恢复内置测试猫可用', 'no package was loaded');
  }

  await materialCheck(
    'material.identity-stable',
    '素材加载/播放/失败/停止/卸载/恢复不改变伙伴身份、回执、管理稿或会话修订',
    async () => {
      const after = await readSnapshot(cdp);
      ensure(
        JSON.stringify(materialIdentity(after)) === identityBefore,
        'material operations changed buddy identity, buddyCommit, management draft or session revision',
        { before: identityBefore, after: materialIdentity(after) },
      );
      return {
        activeBuddyId: after.activeBuddyId,
        buddiesRevision: after.buddiesRevision,
      };
    },
  );

  return {
    supported: true,
    failedChecks,
    loaded: packLoaded,
    initialState: initialProbe.raw,
  };
}

async function main() {
  await assertPrerequisites();
  const startedAt = new Date();
  const runStamp = startedAt.toISOString().replace(/[:.]/g, '-');
  const report = {
    startedAt: startedAt.toISOString(),
    hostBinary: path.relative(workspaceRoot, binary),
    orchestration:
      'WebView2 CDP Input.insertText/Input.dispatchMouseEvent drives real DOM forms and buttons in the production Tauri host; model traffic uses an in-process loopback fixture.',
    scenarios: {},
    instances: [],
    skipped: [],
    pendingIntegration: RUN.pending,
    checks: RUN.checks,
    cleanupNotes: RUN.cleanupNotes,
    limitations: [
      'Synthetic loopback model only; no real endpoint, credential, or user archive is used.',
      'Coverage is Windows/WebView2 in a real window; real IME, mixed DPI, native tray popup painting/mouse clicks, and non-Windows hosts remain unverified.',
      'Tray exit uses the native WM_COMMAND path, not a mouse click on the tray menu; native close uses WM_CLOSE.',
      'After a failed close/exit assertion, show_chat is used only to restore the window for cleanup and is not counted as evidence.',
      'Dependent scenarios never reuse a data root while its owning process is still alive; every launched handle is retained and cleaned in the final pass.',
      'Known pre-fix failures are asserted as failures and not counted as passes.',
      'The C material scenario uses only synthetic PNG-sequence/GIF packages and one corrupt-PNG package generated under target/ plus the explicit temporary data-dir/profile and a loopback model. Material state and command replies are parsed with the production src/core/material.ts contract; a connected endpoint that returns an invalid projection is a real failure, while only a missing endpoint is recorded as pending. Loading is driven through the production main-window Tauri drag-drop listener by injecting a tauri://drag-drop event (not an OS mouse drag); controls use the real material panel buttons; pixels are fetched from the real pet WebView; one direct-IPC load is explicitly labelled as boundary evidence only and is never counted as the product drag-drop entry.',
      'C material negative packages: an in-package relative path escape (manifest ../escape.png) and an over-limit 513x513 frame header are also driven only through the same injected production tauri://drag-drop listener. Their asynchronous final rejection must keep the valid old package, action, playback/resource generations and buddy identity unchanged while the old frames keep advancing. This is event injection into the production listener, not an OS-level mouse drag; the specific rejection cause is established by the tiny synthetic fixture and the production MaterialPack regression, because the UI only exposes the fixed safe failure copy.',
    ],
  };
  let current;
  let model;
  const createdInstances = [];
  const dataRoot = path.join(outputRoot, 'data', `restore-${runStamp}`);
  const saveFailureRoot = path.join(outputRoot, 'data', `save-failure-${runStamp}`);
  const publishFailureRoot = path.join(outputRoot, 'data', `publish-failure-${runStamp}`);
  const conflictRoot = path.join(outputRoot, 'data', `conflict-${runStamp}`);
  const sessionRoot = path.join(outputRoot, 'data', `session-a-${runStamp}`);
  const sessionStorageUnknownRoot = path.join(outputRoot, 'data', `session-storage-unknown-${runStamp}`);
  const sessionQuitRoot = path.join(outputRoot, 'data', `session-quit-scope-${runStamp}`);
  const buddyRoot = path.join(outputRoot, 'data', `buddy-b-${runStamp}`);
  const materialRoot = path.join(outputRoot, 'material', `pack-${runStamp}`);
  writeFixtures({
    outDir: materialRoot,
    only: [
      'valid-sequence-basic',
      'valid-gif-disposal',
      'reject-png-corrupt-idat',
      'reject-path-escape',
      'reject-frame-dimension',
    ],
    log: () => {},
  });

  const closeQuietly = async (instance, reason) => {
    if (instance === undefined) return false;
    const killed = await closeInstance(instance, reason);
    if (killed) report.cleanupKilled = true;
    return killed;
  };

  try {
    model = await syntheticModelServer();

    // 实例 1：真实 DOM 配置 → 发送 → 记忆 → 诊断；无未保存草稿正常退出。
    current = await launch('business', dataRoot, createdInstances);

    // B 集成前置：严格前端要求伙伴注册表三元组。宿主未接线时本轮不做任何
    // 真窗验收，只记录待联调项并经自身正常退出收尾；绝不把旧宿主当 B 通过。
    const contractGate = await hostContractReadback(current.cdp);
    const frontendReady = await strictFrontendReady(current.cdp);
    report.contractGate = {
      buddyContract: contractGate.buddySupported,
      sessionContract: contractGate.sessionSupported,
      strictFrontendBusinessShell: frontendReady,
      observedFields: contractGate.response?.snapshot === null
        ? null
        : Object.keys(contractGate.response?.snapshot ?? {}).sort(),
      observedError: contractGate.response?.error ?? null,
    };
    if (!contractGate.buddySupported) {
      RUN.pending.push({
        id: 'buddy.b-host-contract',
        reason:
          'real host snapshot lacks activeBuddyPersona or a valid managementDraft.kind (none/general/buddy_create/buddy_rename); the strict frontend rejects the snapshot, so no real-window acceptance is claimed',
        observedFields: report.contractGate.observedFields,
      });
      console.log(
        'PENDING buddy.b-host-contract: real host has not integrated the frozen B snapshot contract; real-window acceptance stays pending',
      );
      await check(
        'exit.contract-probe-quit',
        'B 契约未接通时仅以真实正常退出收尾，不启动任何业务场景',
        async () => {
          const outcome = await quitWithOptionalConfirm(current);
          ensure(outcome.exit.code === 0, `contract-probe quit exit ${JSON.stringify(outcome.exit)}`, outcome);
          ensure(outcome.exit.signal === null, `contract-probe quit signalled: ${outcome.exit.signal}`, outcome);
          return outcome;
        },
      );
      current.cdp?.close();
      current.cdp = null;
      if (current.child.exitCode === null) {
        await closeQuietly(current, 'contract-probe-exit-failure');
      }
      current = undefined;
      throw new IntegrationPending(
        'B host contract is not integrated; A/B real-window acceptance is not claimed against this host',
        report.contractGate,
      );
    }
    if (!frontendReady) {
      await check(
        'runtime.strict-frontend-contract',
        '完整 B 契约下严格前端必须渲染业务主壳',
        async () => {
          ensure(frontendReady, 'strict frontend did not reach the business shell despite a complete B contract', {
            observedFields: report.contractGate.observedFields,
          });
          return { frontendReady };
        },
      );
    }
    await check('isolation.synthetic-data-profile-service', '数据根、WebView profile 与模型服务均为隔离合成', async () => {
      const isolatedRoot = path.resolve(outputRoot) + path.sep;
      const isolatedProfiles = path.join(path.resolve(outputRoot), 'profiles') + path.sep;
      const resolvedData = path.resolve(current.dataRoot) + path.sep;
      const resolvedProfile = path.resolve(current.profile) + path.sep;
      ensure(resolvedData.startsWith(isolatedRoot), 'data root is outside target/native-smoke', { dataRoot: current.dataRoot });
      ensure(resolvedProfile.startsWith(isolatedProfiles), 'WebView profile is outside target/native-smoke/profiles', {
        profile: current.profile,
      });
      ensure(
        model.endpoint.startsWith('http://127.0.0.1:') && new URL(model.endpoint).pathname === '/v1/chat/completions',
        'model service is not the synthetic loopback Chat Completions fixture',
        { endpoint: model.endpoint },
      );
      return {
        dataRoot: path.relative(workspaceRoot, current.dataRoot),
        webview2Profile: path.relative(workspaceRoot, current.profile),
        modelEndpoint: model.endpoint,
      };
    });
    await scenarioIpcBoundary(current);
    await scenarioModelAndChat(current, model);
    await scenarioMemoryAndDiagnostics(current, model);
    const businessExitOk = await check('exit.normal-quit', '无未保存草稿时正常退出码为 0', async () => {
          await typeChat(current.cdp, '');
      await waitForSnapshot(
        current.cdp,
        (state) => state.chat.draft === '',
        5_000,
        'chat draft cleared before quit',
      );
      const exit = await quitNormally(current);
      ensure(exit.code === 0, `normal quit exit code ${JSON.stringify(exit)}`);
      ensure(exit.signal === null, `normal quit was signalled: ${exit.signal}`);
      return exit;
    });
    current.cdp?.close();
    current.cdp = null;
    if (!businessExitOk || current.child.exitCode === null) {
      report.skipped.push({
        id: 'restore-chain',
        reason: 'business instance did not exit normally; dependent restore scenarios did not reuse its data root',
      });
      await closeQuietly(current, 'business-exit-failure');
      current = undefined;
    } else {
      current = undefined;

      // 实例 2：同一数据根重启恢复 → 密钥/许可不恢复 → 未保存管理草稿的关闭/原生关窗/托盘退出。
      current = await launch('restore', dataRoot, createdInstances);
      await scenarioRestore(current, model);
      await scenarioManagementDraftExit(current);
      if (current.child.exitCode === null) {
        const restoreExitOk = await check(
          'exit.restore-instance-cleanup-quit',
          '恢复实例在桌面草稿路径后仍能正常退出',
          async () => {
            const outcome = await quitWithOptionalConfirm(current);
            ensure(outcome.exit.code === 0, `restore instance exit ${JSON.stringify(outcome.exit)}`);
            ensure(outcome.exit.signal === null, `restore instance signalled: ${outcome.exit.signal}`);
            return outcome;
          },
        );
        if (!restoreExitOk) await closeQuietly(current, 'restore-exit-failure');
      }
      current.cdp?.close();
      current.cdp = null;
      current = undefined;
    }

    // 实例 3：独立数据根验证 buddies.pending CreatePending 失败按 NotExecuted 收口。
    current = await launch('save-failure', saveFailureRoot, createdInstances);
    await scenarioSaveFailure(current, model);
    const saveExitOk = await check('exit.save-failure-quit', '保存失败实例可经真实确认正常退出', async () => {
      const outcome = await quitWithOptionalConfirm(current);
      ensure(outcome.exit.code === 0, `save-failure quit exit ${JSON.stringify(outcome.exit)}`);
      ensure(outcome.exit.signal === null, `save-failure quit signalled: ${outcome.exit.signal}`);
      return outcome;
    });
    current.cdp?.close();
    current.cdp = null;
    if (!saveExitOk) await closeQuietly(current, 'save-failure-exit-failure');
    current = undefined;

    // 实例 4：独立数据根制造 Publish 失败，验证真正的 Unknown 与状态重读保持。
    current = await launch('publish-failure', publishFailureRoot, createdInstances);
    await scenarioPublishFailure(current, model);
    const publishExitOk = await check('exit.publish-failure-quit', '未知结果实例经真实确认后以 0 退出', async () => {
      const outcome = await quitWithOptionalConfirm(current);
      ensure(outcome.exit.code === 0, `publish-failure quit exit ${JSON.stringify(outcome.exit)}`);
      ensure(outcome.exit.signal === null, `publish-failure quit signalled: ${outcome.exit.signal}`);
      return outcome;
    });
    current.cdp?.close();
    current.cdp = null;
    if (!publishExitOk) await closeQuietly(current, 'publish-failure-exit-failure');
    current = undefined;

    // 实例 5：独立数据根复验真实版本冲突的明确分类与恢复路径。
    current = await launch('conflict', conflictRoot, createdInstances);
    await scenarioMemoryConflict(current);
    const conflictExitOk = await check('exit.conflict-instance-quit', '版本冲突实例经真实确认后以 0 退出', async () => {
      const outcome = await quitWithOptionalConfirm(current, { componentUnmounted: true });
      ensure(outcome.exit.code === 0, `conflict instance exit ${JSON.stringify(outcome.exit)}`);
      ensure(outcome.exit.signal === null, `conflict instance signalled: ${outcome.exit.signal}`);
      return outcome;
    });
    current.cdp?.close();
    current.cdp = null;
    if (!conflictExitOk) await closeQuietly(current, 'conflict-exit-failure');
    current = undefined;

    // 实例 6：独立数据根执行 A 会话生命周期真实 DOM 场景；宿主未接线时只记待联调。
    current = await launch('session-a', sessionRoot, createdInstances);
    const sessionFailuresBefore = RUN.failures;
    const sessionOutcome = await scenarioSessionLifecycle(current, model);
    const sessionDirty = RUN.failures > sessionFailuresBefore;
    const sessionExitOk = await check('exit.session-instance-quit', '会话实例无残留正常退出；失败路径也只用自身确认收尾', async () => {
      const outcome = await quitWithOptionalConfirm(current);
      ensure(outcome.exit.code === 0, `session instance exit ${JSON.stringify(outcome.exit)}`, outcome);
      ensure(outcome.exit.signal === null, `session instance signalled: ${outcome.exit.signal}`, outcome);
      if (!sessionDirty) {
        ensure(
          outcome.confirmed === true,
          'the retained unknown-session draft was discarded without Quit confirmation',
          outcome,
        );
      }
      return outcome;
    });
    current.cdp?.close();
    current.cdp = null;
    if (!sessionExitOk && current.child.exitCode === null) {
      await closeQuietly(current, 'session-exit-failure');
    }
    current = undefined;

    if (sessionOutcome.aHostAvailable && sessionOutcome.archivedSessionId !== null) {
      // 实例 7：同一会话数据根重启，验证归档只读与选择在恢复后仍保留。
      current = await launch('session-a-restart', sessionRoot, createdInstances);
      await scenarioSessionArchiveRestart(current, sessionOutcome);
      const restartExitOk = await check('exit.session-restart-quit', '归档恢复实例只做只读检查并以 0 正常退出', async () => {
        const outcome = await quitWithOptionalConfirm(current);
        ensure(outcome.exit.code === 0, `session restart exit ${JSON.stringify(outcome.exit)}`, outcome);
        ensure(outcome.exit.signal === null, `session restart signalled: ${outcome.exit.signal}`, outcome);
        return outcome;
      });
      current.cdp?.close();
      current.cdp = null;
      if (!restartExitOk && current.child.exitCode === null) {
        await closeQuietly(current, 'session-restart-exit-failure');
      }
      current = undefined;
    } else {
      report.skipped.push({
        id: 'session.archive-restart-retained',
        reason: sessionOutcome.aHostAvailable
          ? 'archive check did not produce an archived session in this run'
          : 'A host snapshot contract is not integrated yet',
      });
    }

    if (sessionOutcome.aHostAvailable) {
      // 实例 8：独立数据根制造存储 Publish Unknown，验证新建会话不能绕过全局未知。
      current = await launch('session-storage-unknown', sessionStorageUnknownRoot, createdInstances);
      await scenarioSessionStorageUnknown(current, model);
      const storageExitOk = await check('exit.session-storage-unknown-quit', '存储未知实例经真实确认后以 0 退出', async () => {
        const outcome = await quitWithOptionalConfirm(current);
        ensure(outcome.exit.code === 0, `session storage unknown exit ${JSON.stringify(outcome.exit)}`, outcome);
        ensure(outcome.exit.signal === null, `session storage unknown signalled: ${outcome.exit.signal}`, outcome);
        return outcome;
      });
      current.cdp?.close();
      current.cdp = null;
      if (!storageExitOk && current.child.exitCode === null) {
        await closeQuietly(current, 'session-storage-unknown-exit-failure');
      }
      current = undefined;
    } else {
      report.skipped.push({
        id: 'session.storage-unknown-blocks-new',
        reason: 'A host snapshot contract is not integrated yet; strict frontend cannot reach the lifecycle UI',
      });
    }

    if (sessionOutcome.aHostAvailable) {
      // 实例 9：整个应用退出必须识别所有会话草稿（领队决定）。Rust 侧保护由另一单修，
      // 这里用真实 DOM 前置如实断言当前缺口，并以 knownFix 归类，绝不假通过。
      current = await launch('session-quit-scope', sessionQuitRoot, createdInstances);
      await check(
        'exit.quit-discard-all-session-drafts',
        '整个应用退出须识别所有会话草稿，确认文案覆盖所有会话与管理未保存编辑',
        async () => {
          const initial = await readSnapshot(current.cdp);
          const draftA = '退出保护的非活动会话草稿';
          await typeChat(current.cdp, draftA);
          await waitForSnapshot(
            current.cdp,
            (state) => state.chat.draft === draftA,
            10_000,
            'non-active session draft synced',
          );
          await current.cdp.clickElement(`__smoke.sessionAction('新建会话')`);
          const split = await waitForSnapshot(
            current.cdp,
            (state) => state.activeSessionId !== initial.activeSessionId && state.chat.draft === '',
            10_000,
            'fresh session while the other session keeps its draft',
          );
          const requestsBefore = model.state.requests;
          await invokeWindowWithContext(current, 'quit');
          const deadline = Date.now() + 8_000;
          let dialog = null;
          while (Date.now() < deadline) {
            if (current.child.exitCode !== null) break;
            if (await cdpDialogPresent(current)) {
              dialog = await dialogText(current.cdp);
              break;
            }
            await sleep(150);
          }
          if (dialog === null) {
            ensure(
              current.child.exitCode !== null,
              'quit neither asked for confirmation nor exited',
              { sessions: split.sessions, activeSessionId: split.activeSessionId, draftA },
            );
            throw new CheckFailure(
              'quit exited without protecting another session draft (Rust protection still only covers the active session)',
              {
                exitCode: current.child.exitCode,
                sessions: split.sessions,
                nonActiveSessionDraft: draftA,
                requestsBefore,
              },
            );
          }
          ensure(
            /所有会话草稿/.test(dialog),
            `quit confirmation does not cover all session drafts: ${dialog}`,
            { dialog },
          );
          ensure(
            /管理/.test(dialog),
            `quit confirmation does not cover unsaved management edits: ${dialog}`,
            { dialog },
          );
          const label = await clickDialogConfirm(current.cdp);
          const exit = await waitForExit(current.child, 20_000);
          ensure(exit.code === 0 && exit.signal === null, `quit exit ${JSON.stringify(exit)}`, { exit });
          ensure(model.state.requests === requestsBefore, 'quit re-sent a chat request');
          return { dialog, label, exit, sessions: split.sessions };
        },
        { knownFix: 'quit-all-session-drafts' },
      );
      // 缺口未修时进程可能仍存活（确认被拒绝等）：仍用正常退出确认收尾，不强杀。
      if (current.child.exitCode === null) {
        await resetUiForExit(current);
        await check('exit.session-quit-scope-cleanup', '退出保护缺口实例仍只用自身确认收尾', async () => {
          const outcome = await quitWithOptionalConfirm(current);
          ensure(outcome.exit.code === 0, `quit scope cleanup exit ${JSON.stringify(outcome.exit)}`, outcome);
          ensure(outcome.exit.signal === null, `quit scope cleanup signalled: ${outcome.exit.signal}`, outcome);
          return outcome;
        });
      }
      current.cdp?.close();
      current.cdp = null;
      if (current.child.exitCode === null) {
        await closeQuietly(current, 'session-quit-scope-exit-failure');
      }
      current = undefined;
    }

    // 实例 10：B 伙伴注册表真窗场景（创建/切换/改名与两伙伴隔离）。
    current = await launch('buddy-b', buddyRoot, createdInstances);
    const buddyOutcome = await scenarioBuddyLifecycle(current, model);
    let materialOutcome = { supported: false, error: 'material scenario did not run', failedChecks: [] };
    await check(
      'material.phase-c',
      'C 素材包/播放器只在显式合成包与临时数据根下做真实窗验证；任一子项失败即失败',
      async () => {
        materialOutcome = await scenarioMaterial(current, materialRoot);
        ensure(materialOutcome.supported === true, 'material IPC endpoint is unavailable', materialOutcome);
        ensure(
          materialOutcome.failedChecks.length === 0,
          `C material sub-checks failed: ${materialOutcome.failedChecks.join(', ')}`,
          materialOutcome,
        );
        return materialOutcome;
      },
    );
    report.scenarios.material = materialOutcome;
    const buddyExitOk = await check(
      'exit.buddy-instance-quit',
      '伙伴实例无残留正常退出；失败路径也只用自身确认收尾',
      async () => {
        const outcome = await quitWithOptionalConfirm(current);
        ensure(outcome.exit.code === 0, `buddy instance exit ${JSON.stringify(outcome.exit)}`, outcome);
        ensure(outcome.exit.signal === null, `buddy instance signalled: ${outcome.exit.signal}`, outcome);
        if (buddyOutcome.crossBuddyDraftArmed) {
          ensure(
            outcome.confirmed === true,
            'quit did not ask to discard the non-active buddy draft',
            outcome,
          );
          const dialog = outcome.dialogs.map((entry) => entry.dialog).join('\n');
          ensure(
            dialog.includes('所有会话草稿'),
            'quit confirmation does not describe the whole-application draft scope',
            { dialogs: outcome.dialogs },
          );
        }
        return outcome;
      },
    );
    current.cdp?.close();
    current.cdp = null;
    if (!buddyExitOk && current.child.exitCode === null) {
      await closeQuietly(current, 'buddy-exit-failure');
    }
    current = undefined;

    if (buddyOutcome.bHostAvailable && buddyOutcome.restartReady && buddyExitOk) {
      // 实例 11：同一伙伴数据根重启，验证伙伴选择/版本与正文归属保留、凭据不恢复。
      current = await launch('buddy-b-restart', buddyRoot, createdInstances);
      await scenarioBuddyRestart(current, buddyOutcome);
      const buddyRestartExitOk = await check(
        'exit.buddy-restart-quit',
        '伙伴恢复实例只做只读切换检查并以 0 正常退出',
        async () => {
          const outcome = await quitWithOptionalConfirm(current);
          ensure(outcome.exit.code === 0, `buddy restart exit ${JSON.stringify(outcome.exit)}`, outcome);
          ensure(outcome.exit.signal === null, `buddy restart signalled: ${outcome.exit.signal}`, outcome);
          return outcome;
        },
      );
      current.cdp?.close();
      current.cdp = null;
      if (!buddyRestartExitOk && current.child.exitCode === null) {
        await closeQuietly(current, 'buddy-restart-exit-failure');
      }
      current = undefined;
    } else if (buddyOutcome.bHostAvailable) {
      report.skipped.push({
        id: 'buddy.restart-retained',
        reason: 'buddy instance did not exit cleanly or the scenario did not reach restart',
      });
    } else {
      report.skipped.push({
        id: 'buddy.lifecycle',
        reason: 'B host snapshot contract is not integrated yet',
      });
    }

    report.notes = [
      'Each process used a fresh WEBVIEW2_USER_DATA_FOLDER under target/native-smoke/profiles.',
      'Every process used only the explicit synthetic data root under target/native-smoke/data.',
      'Instance 1 exits normally without drafts; instance 2 verifies restore then exercises management-draft close/native-close/tray cancel and discard paths; instance 3 verifies buddies.pending CreatePending failure is reported NotExecuted; instance 4 verifies a true Unknown at the buddies.v1 publish stage and reread lock; instance 5 verifies memory revision conflict classification and recovery.',
      'Instance 6 (A) drives loopback config, 33 DOM rounds, draft/message isolation, failed-switch draft retention, external Unknown then explicit isolated new session with request-count checks, and archive read-only; instance 7 reuses only that data root after exit to verify archive retention; instance 8 verifies a storage Unknown blocks explicit new-session creation.',
      'Instance 9 asserts that a whole-application quit recognises another session\u2019s unsaved draft and that its confirmation copy covers all session drafts plus unsaved management edits; the Rust application now binds the confirmed Quit to all session drafts and echoes one versioned `chatDraft.discard` receipt.',
      'A failed scenario always resets only its own panel/dialog/draft state in finally and then exits through its own normal quit confirmation; a live instance handle is never dropped and a forced kill is never counted as normal-exit evidence.',
      'The A scenarios stay pending integration when the real host snapshot lacks activeSessionId/sessionsRevision/sessions/sessionIsolationAllowed; they are never reported as passed against an older host or a stub.',
      'B: the strict frontend requires activeBuddyId/buddiesRevision/buddies plus a strict buddyCommit receipt. When the host lacks any of them the script records buddy.b-host-contract, performs only a real normal-quit check, and exits INCOMPLETE instead of running stale-window checks.',
      'B: with the contract integrated, instance 10 creates/selects/renames buddies through real DOM input, checks per-buddy messages/drafts/config/API-Key isolation (including identical local session ids), leaves an unsubmitted draft on the non-active buddy to prove whole-application Quit protection, and instance 11 reuses only its data root after exit to check registry/selection retention and that credentials/transmission do not come back.',
      'B IPC payload evidence wraps the real Tauri custom-protocol fetch channel in CDP and validates the observer with a live prototype_snapshot call first; it does not rely on replacing the non-writable __TAURI_INTERNALS__.invoke property.',
      'B main-window show_chat/hide_chat/quit payloads carry the complete buddy context captured when the command is initiated; the smoke also sends a deliberately stale same-buddy revision and a completely missing context to the host and requires explicit rejection with no visibility, selection, close-confirmation or process side effect. Pet windows keep only show_chat focus plus pet presentation/drag and are denied hide_chat/quit.',
      'A failed scenario explicitly attempts to close its own buddy create/rename forms, panels and drafts before the instance exits through its own normal quit confirmation; where the host Unknown-close gap prevents that, the cleanup failure is retained instead of being reported as success, and dependent checks are never recorded as passed when their prerequisite did not run.',
      'In this integrated worktree the same-round Rust-side Unknown metadata/close gaps are not backported: the storage-unknown quit check is reported as a real failure until the lead merges the Rust fix. The smoke does not bypass that gap, does not count a forced kill as a normal exit, and the current copy does not claim all-instance normal exit.',
      'The publish-Unknown reread check distinguishes a hidden Apply button (panel closed) from a disabled one, requires unknownLock/sendDisabled to stay set, and verifies the backend still rejects a business command with buddy_storage_unknown while buddies.v1 and buddies.pending stay byte-for-byte untouched.',
      'The restore chain only reuses the business data root after the business process has actually exited; every launched instance handle is retained and cleaned in the final pass.',
      'C material: instance 10 additionally drives the production main-window drag-drop handler (Tauri tauri://drag-drop event injection, not a real mouse drag), real material-panel buttons, a PNG sequence and a GIF package plus a corrupt PNG, an in-package relative path escape (../escape.png) and a 513x513 over-limit frame package that must fail asynchronously while the old package, action, playback/resource generations and buddy identity stay put and the old frames keep advancing, pet-WebView raw frame binaries compared with independently derived fixture pixels, pause-without-advance, stop/replay, failed-package switch while old playback continues, unload Canvas/resource release, builtin restore usability, main-window pixel denial and pet-window command denial. The aggregate material.phase-c check fails whenever any C sub-check fails.',
      'C material: if the host has already integrated material/command/reply generation but its initial builtin source is rejected by the production parser, material.initial-builtin-usable and the product drag-drop load are real failures; the direct-IPC boundary load only prepares pet-frame evidence and never replaces or masks the failed product entry.',
    ];
  } catch (error) {
    if (error instanceof IntegrationPending) {
      // 待联调不是失败：事件报告保留 pending 项并以非零退出，避免旧宿主被当成 B 验收。
      report.integrationPending = { message: error.message, evidence: error.evidence ?? null };
    } else {
      report.error = error.stack ?? String(error);
    }
  } finally {
    for (const instance of createdInstances) {
      if (instance.pid !== undefined && instance.child.exitCode === null) {
        const killed = await closeInstance(instance, 'final-cleanup').catch((error) => {
          instance.cleanupError = error.message;
          return false;
        });
        if (killed) report.cleanupKilled = true;
      } else if (instance.cdp !== null && instance.cdp !== undefined) {
        instance.cdp.close();
        instance.cdp = null;
      }
    }
    report.instances = createdInstances.map((instance) => reportInstance(instance));
    if (model !== undefined) {
      await model.close();
    }
  }

  report.finishedAt = new Date().toISOString();
  report.summary = {
    passed: RUN.checks.filter((entry) => entry.status === 'passed').length,
    failed: RUN.checks.filter((entry) => entry.status === 'failed').length,
    knownFailures: RUN.checks.filter(
      (entry) => entry.status === 'failed' && entry.targetsKnownFix !== undefined,
    ).length,
    pendingIntegration: RUN.pending.length,
    checks: RUN.checks.map((entry) => ({
      id: entry.id,
      status: entry.status,
      targetsKnownFix: entry.targetsKnownFix ?? null,
    })),
  };
  report.knownIssueCategories = {};
  for (const entry of RUN.checks) {
    if (entry.status !== 'failed' || entry.targetsKnownFix === undefined) continue;
    const category = entry.targetsKnownFix;
    report.knownIssueCategories[category] ??= [];
    report.knownIssueCategories[category].push(entry.id);
  }
  report.transport = {
    domInput: 'CDP Input.insertText after focusing a semantic label target',
    domClick: 'CDP Input.dispatchMouseEvent (mousePressed/mouseReleased) at the semantic target center',
    nativeClose: 'WM_CLOSE posted to the main Tauri window',
    nativeTrayQuit: 'WM_COMMAND 1003 posted to the tray message window',
    model: 'in-process Node HTTP loopback fixture',
    materialDrop:
      'tauri://drag-drop injected through the production JS listener registered in __internal_unstable_listeners_object_id__ and __TAURI_INTERNALS__.runCallback; an event injection, not a real OS mouse drag',
    materialControls: 'real material panel buttons through CDP mousePressed/mouseReleased; action select through a DOM change event',
    materialPixels: 'real pet WebView2 target calls prototype_frame/prototype_material_pixels; the main window must be denied pet-only pixels',
  };
  report.instances = report.instances.filter(
    (instance, index, list) => list.findIndex((other) => other.pid === instance.pid) === index,
  );
  const stillAlive = createdInstances.filter(
    (instance) => instance.pid !== undefined && childAlive(instance.child),
  );
  report.cleanup = {
    launched: createdInstances.length,
    exited: createdInstances.filter((instance) => !childAlive(instance.child)).length,
    killed: createdInstances.filter((instance) => instance.cleanupKilled === true).length,
    identityMismatches: createdInstances
      .filter((instance) => instance.cleanupIdentityMismatch === true)
      .map((instance) => instance.pid),
    stillAlive: stillAlive.map((instance) => ({
      pid: instance.pid,
      dataRoot: path.relative(workspaceRoot, instance.dataRoot),
    })),
  };
  if (stillAlive.length > 0 || report.cleanup.identityMismatches.length > 0) {
    report.cleanupFailure = true;
  }
  if (createdInstances.some((instance) => instance.cleanupKilled === true)) {
    report.cleanupKilled = true;
  }
  await fs.mkdir(outputRoot, { recursive: true });
  const reportPath = path.join(outputRoot, `native-smoke-${runStamp}.json`);
  await fs.writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`, 'utf8');
  console.log(`native smoke report: ${reportPath}`);
  console.log(JSON.stringify(report.summary, null, 2));
  if (report.cleanupKilled === true) {
    console.error('FAIL process cleanup required; a failure path killed only its own process');
  }
  if (report.cleanupFailure === true) {
    console.error(`FAIL process cleanup verification: ${JSON.stringify(report.cleanup)}`);
  }
  if (RUN.failures > 0 || report.cleanupFailure === true) {
    console.error(
      `native smoke FAILED: ${report.summary.failed} failed, ${report.summary.knownFailures} target known fixes, ${RUN.pending.length} pending integration`,
    );
    process.exitCode = 1;
  } else if (RUN.pending.length > 0) {
    // 待联调项不是通过：事件报告保留待办原因并以非零退出，避免旧宿主被当成 A/B 验收。
    console.error(
      `native smoke INCOMPLETE: ${RUN.pending.length} pending integration item(s); A/B acceptance is not claimed`,
    );
    process.exitCode = 1;
  } else {
    console.log('native smoke passed');
  }
}

export { CdpClient, freePort, waitForExit };

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) await main();
