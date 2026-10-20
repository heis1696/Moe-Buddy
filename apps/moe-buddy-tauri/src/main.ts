import { createApp } from 'vue';
import MainView from './app/MainView.vue';
import NoticeView from './app/NoticeView.vue';
import PetView from './app/PetView.vue';
import { currentWindowLabel, runningInTauri } from './platform/tauri';
import { resolveWindowPage } from './core/windowLabel';
import './styles/base.css';

function showBootFailure(): void {
  const root = document.getElementById('app');
  if (root === null) return;
  document.documentElement.dataset.window = 'unknown';
  document.title = 'Moe Buddy · 启动失败';
  root.textContent = 'Moe Buddy 启动失败，请从桌面应用重新启动。';
}

function mountApp(): void {
  const root = document.getElementById('app');
  if (root === null) return;

  // 普通浏览器不启用任何模拟 Tauri 通道，只提示从桌面启动。
  if (!runningInTauri()) {
    document.documentElement.dataset.window = 'browser';
    document.title = 'Moe Buddy · 请从桌面启动';
    createApp(NoticeView, {
      title: '请从桌面启动',
      message: 'Moe Buddy 是桌面应用，请从桌面启动后使用离线原型。',
    }).mount(root);
    return;
  }

  const label = currentWindowLabel();
  const page = resolveWindowPage(label);

  if (page === 'pet') {
    document.documentElement.dataset.window = 'pet';
    document.title = 'Moe Buddy · 宠物';
    createApp(PetView).mount(root);
    return;
  }

  if (page === 'main') {
    document.documentElement.dataset.window = 'main';
    document.title = 'Moe Buddy · 离线验证';
    createApp(MainView).mount(root);
    return;
  }

  document.documentElement.dataset.window = 'unknown';
  document.title = 'Moe Buddy';
  createApp(NoticeView, {
    title: '未识别的窗口',
    message: `当前窗口 label 为“${label ?? '未知'}”，原型只支持 main 与 pet。`,
    hint: '请从桌面应用主窗口重新打开聊天或宠物窗口。',
  }).mount(root);
}

try {
  mountApp();
} catch {
  showBootFailure();
}
