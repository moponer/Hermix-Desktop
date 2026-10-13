import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  // @hermix/preview 的用例经 node_modules 符号链接引用（与构建侧同一路
  // 径），并保留符号链接解析——若按真实路径（packages/preview/src）加载，
  // 包内 `import "xlsx"` 等裸导入从文件位置向上找不到 node_modules。
  resolve: { preserveSymlinks: true },
  test: {
    environment: "jsdom",
    globals: true,
    // enable @testing-library/jest-dom matchers
    // (`toBeInTheDocument` etc.) so React component tests can use
    // them. Without this, every test that touches the DOM would
    // error with "Invalid Chai property".
    setupFiles: ["./vitest.setup.ts"],
    include: [
      "src/**/*.test.{ts,tsx}",
      "node_modules/@hermix/preview/src/**/*.test.{ts,tsx}",
    ],
    // 注意不能用 "**/node_modules/**"（vitest 默认值）——那会把上面的
    // 包用例一起排除掉；这里显式给窄排除。
    exclude: ["dist/**"],
  },
});
