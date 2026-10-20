/*
 * 在首帧绘制前给 html 打上窗口标记，避免透明宠物窗出现白底闪烁。
 * 页面路由仍由 src/main.ts 通过 Tauri API 判定，此脚本只影响初始背景。
 */
(function () {
  try {
    var internals = window.__TAURI_INTERNALS__;
    var metadata = internals && internals.metadata;
    var current = metadata && metadata.currentWindow;
    var label = current && current.label;
    if (label === 'pet' || label === 'main') {
      document.documentElement.dataset.window = label;
    }
  } catch (error) {
    // 非 Tauri 环境或注入结构变化时忽略，由 main.ts 处理提示。
  }
})();
