import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import path from "path";
// Fix the satteri wasm worker's bare-specifier URL in node_modules before
// anything resolves it (dev pre-bundling and production build both load this
// config). Idempotent — see scripts/patch-satteri-wasm.mjs.
import "./scripts/patch-satteri-wasm.mjs";

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
    // 必须显式绑 IPv4 回环（2026-10-27）。默认 host 是 `localhost`，而 Node 17+
    // 的 DNS 顺序是 verbatim（localhost 优先解析成 ::1），vite 因此只监听
    // `[::1]:1420`；src-tauri/src/lib.rs 的 dev_server_reachable() 探的是
    // `127.0.0.1:1420`（= tauri.conf.json 的 devUrl）→ 永远不通 → 启动必弹
    // 「前端 dev server 没有运行」。写死 IPv4 后两端同源，不再依赖解析顺序。
    host: "127.0.0.1",
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
    // 懒加载链上的依赖必须启动时预打包（include），否则首次用到时才被
    // 运行时发现 → 中途重新优化 → 旧 hash 文件失效 → webview 拿过期地址
    // 加载失败 → 白屏（2026-10-15 dev 白屏）。plugin-fs 经 desktopBridge
    // 动态链引入；docx-preview（.docx 渲染，含 peer docx）是纯懒加载入口。
    include: ["@tauri-apps/plugin-fs", "docx-preview"],
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
    rollupOptions: {
      output: {
        // 按「谁在用」分组，而不是把一切塞进 App chunk：
        // - echarts / katex / refractor 体积大但只有少数页面用到，单独成 chunk
        //   后只在真正 import 时取（MemoryTab / 公式 / 代码块）。
        // - react 系独立：改业务代码不会让这份 chunk 失效，热更与增量加载都受益。
        // 不要把懒加载页面自己的代码并进来 —— 那会让首次进入该页时多拉一份。
        manualChunks(id: string) {
          if (!id.includes("node_modules")) return undefined;
          const p = id.replace(/\\/g, "/");
          if (/\/node_modules\/(echarts|zrender)\//.test(p)) return "vendor-echarts";
          if (/\/node_modules\/katex\//.test(p)) return "vendor-katex";
          if (/\/node_modules\/(refractor|prismjs)\//.test(p)) return "vendor-highlight";
          if (/\/node_modules\/(react|react-dom|scheduler)\//.test(p)) return "vendor-react";
          return undefined;
        },
      },
    },
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
