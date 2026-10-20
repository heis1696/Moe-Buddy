import { defineConfig } from 'vite';
import vue from '@vitejs/plugin-vue';

// Tauri 宿主固定通过 http://127.0.0.1:1420 加载开发页，端口被占用时应直接失败而不是漂移。
export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  server: {
    host: '127.0.0.1',
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
  build: {
    // Windows WebView2 跟随系统更新；原型只要求现代 Chromium 能力。
    target: 'chrome105',
  },
});
