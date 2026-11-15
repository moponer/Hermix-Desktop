import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// 查看器前端：与 installer 同策略——刻意最小化（无 Tailwind / 无别名），
// 设计语言靠 styles.css 的语义 token 与主应用同族，不共享构建链。
export default defineConfig({
  plugins: [react()],
  clearScreen: false,
  base: "./",
  server: { port: 1431, strictPort: true },
  esbuild: { target: "es2022" },
  // 预览引擎（@hermix/preview barrel）走懒加载，dev 下这些包只能从动态
  // 链被发现 → 首次用到时才预打包 → 中途重新优化 → 旧 hash 失效 →
  // webview 拿过期地址加载失败 → 白屏（桌面端 2026-10-15 dev 白屏的同款
  // 教训，见桌面端 vite.config.ts 的注释）。故启动时预打包。
  optimizeDeps: {
    esbuildOptions: { target: "es2022" },
    include: [
      "docx-preview",
      "exceljs",
      "pdfjs-dist",
      "pptx-browser",
      "xlsx",
      "papaparse",
    ],
    // satteri 的浏览器 wasm 链用 `new URL("./x.wasm", import.meta.url)` 取
    // wasm 字节。预打包会把 import.meta.url 改写成 .vite/deps/ 下的路径，
    // 相对 wasm 在该目录不存在 → dev server 回退返回 index.html → WASM 报
    // "expected magic word"。排除后 vite 按 /@fs/ 路径原样服务真实 .wasm。
    // 仅影响 dev 预打包；production build 仍由 rollup 正常 emit wasm 资源。
    exclude: [
      "satteri",
      "@bruits/satteri-wasm32-wasi",
      "@napi-rs/wasm-runtime",
      "@emnapi/core",
      "@emnapi/runtime",
      "@emnapi/wasi-threads",
      "@tybys/wasm-util",
    ],
  },
  build: {
    target: "es2022",
    // 与 src-tauri/tauri.conf.json 的 frontendDist(../dist) 一致——
    // generate_context! 编译期把这里嵌入 exe，路径错位 = 嵌入旧 UI。
    outDir: "dist",
    emptyOutDir: true,
    // 预览引擎 chunk（PreviewHost + exceljs + pdf.js）~1MB 是**懒加载**的，
    // 不进首屏；默认 500KB 警告线对它是噪音，放宽到 2600（与桌面端一致）。
    chunkSizeWarningLimit: 2600,
  },
});
