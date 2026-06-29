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
    // 2026-06-20: 前端产物落到外层 release/frontend/, 不再污染
    // apps/desktop-tauri/. tauri.conf.json 的 frontendDist 也跟着
    // 改成 "../../../release/frontend" 同步. 路径相对 vite.config.ts
    // 所在目录 (即 apps/desktop-tauri/), 所以 ../../release/frontend
    // 等价于 repo root/release/frontend.
    outDir: "../../release/frontend",
    emptyOutDir: true,
  },
});
