import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

export default defineConfig({
  plugins: [react()],
  test: {
    environment: "jsdom",
    globals: true,
    // enable @testing-library/jest-dom matchers
    // (`toBeInTheDocument` etc.) so React component tests can use
    // them. Without this, every test that touches the DOM would
    // error with "Invalid Chai property".
    setupFiles: ["./vitest.setup.ts"],
    include: ["src/**/*.test.{ts,tsx}"],
    exclude: ["node_modules", "dist"],
  },
});
