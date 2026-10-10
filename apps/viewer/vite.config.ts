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
  build: {
    target: "es2022",
    // 与 src-tauri/tauri.conf.json 的 frontendDist(../dist) 一致——
    // generate_context! 编译期把这里嵌入 exe，路径错位 = 嵌入旧 UI。
    outDir: "dist",
    emptyOutDir: true,
  },
});
