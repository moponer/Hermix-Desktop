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

// jsdom 环境下 window.localStorage 可能未定义（storageCompat.ts 直接访问
// window.localStorage，48 个用例实测因此挂掉）；WebView2 真机必有，这里补
// 最小内存实现，与上面的 ResizeObserver stub 同款思路。
if (typeof window !== "undefined" && typeof window.localStorage === "undefined") {
  const store = new Map<string, string>();
  const storage = {
    getItem: (key: string) => (store.has(key) ? (store.get(key) as string) : null),
    setItem: (key: string, value: string) => void store.set(String(key), String(value)),
    removeItem: (key: string) => void store.delete(key),
    clear: () => void store.clear(),
    key: (index: number) => Array.from(store.keys())[index] ?? null,
    get length() {
      return store.size;
    },
  };
  Object.defineProperty(window, "localStorage", { value: storage, configurable: true });
}