import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "path";

export default defineConfig({
  plugins: [react()],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  clearScreen: false,
  base: "./",
  server: {
    port: 1420,
    strictPort: true,
  },
  build: {
    outDir: "../../release/frontend",
    emptyOutDir: true,
    // 主 JS bundle ~2.4MB (桌面本地加载, WebView2 读本地文件无感)。
    // 默认 500KB 警告线对桌面应用不适用, 放宽到 2600KB 消掉 advisory warning。
    chunkSizeWarningLimit: 2600,
    // Remove console.* in production builds to reduce bundle size and
    // prevent debug info leakage. Warnings/errors are kept for
    // troubleshooting via the DevTools console in dev mode.
    terser: {
      compress: {
        drop_console: true,
        drop_debugger: true,
      },
    },
  },
});
