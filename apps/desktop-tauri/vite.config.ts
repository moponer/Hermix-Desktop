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
  // WebView2 (Chromium 100+) 原生支持 top-level await。默认 es2020 目标会让
  // satteri 的 wasm loader 在 esbuild 阶段直接报 "Top-level await is not available"。
  esbuild: {
    target: "es2022",
  },
  optimizeDeps: {
    esbuildOptions: {
      target: "es2022",
    },
    // satteri 的浏览器 wasm 链用 `new URL("./x.wasm", import.meta.url)` 取
    // wasm 字节。预打包(esbuild optimizeDeps)会把 `import.meta.url` 改写成
    // `.vite/deps/` 下的路径，相对 wasm 在该目录不存在 → dev server 的 SPA
    // 回退返回 index.html(<!doctype) → WebAssembly.instantiate 报
    // "expected magic word ... found 3c 21 64 6f"。排除后 vite 按
    // /@fs/.../node_modules 原样服务真实 .wasm 文件，dev 才可加载。
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
