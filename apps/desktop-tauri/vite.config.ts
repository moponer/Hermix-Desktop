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
