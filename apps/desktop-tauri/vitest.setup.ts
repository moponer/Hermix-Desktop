/**
 * vitest setup — wires @testing-library/jest-dom so every test file
 * gets the extended DOM matchers (`toBeInTheDocument`,
 * `toHaveTextContent`, etc.) for free.
 *
 * React component tests for `CompactionStatusBanner` unblocked by this.
 * Without it, tests error with "Invalid Chai property: toBeInTheDocument"
 * because the matcher lives in a separate package.
 */
import "@testing-library/jest-dom/vitest";

// jsdom 没有 ResizeObserver（TableView 虚拟滚动视口测量依赖它）；
// WebView2 真机必有，这里仅补测试环境 stub。
if (typeof globalThis.ResizeObserver === "undefined") {
  class ResizeObserverStub {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  (globalThis as { ResizeObserver?: unknown }).ResizeObserver = ResizeObserverStub;
}