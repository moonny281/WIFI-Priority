import { defineConfig } from 'vite';
import { fileURLToPath, URL } from 'node:url';

// 两个页面：主窗口 index.html，桌面悬浮框 overlay.html
export default defineConfig({
  clearScreen: false,
  server: { port: 1420, strictPort: true },
  build: {
    target: 'chrome105', // WebView2 基于 Chromium
    rollupOptions: {
      input: {
        main: fileURLToPath(new URL('./index.html', import.meta.url)),
        overlay: fileURLToPath(new URL('./overlay.html', import.meta.url)),
      },
    },
  },
});
