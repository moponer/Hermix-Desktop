import { describe, it, expect } from "vitest";
import { currentZoom, computeMainMinWidth, computeLeftMin } from "./viewport";

// B6-2（docs/optimization-plan.md H7）：视口断点纯函数测试——从 App.tsx
// 提取时锁行为。jsdom 的 documentElement.style.zoom 默认为空，currentZoom
// 回退 1，因此下述断言即 zoom=1 的分档语义。

describe("viewport 断点映射（B6-2 提取自 App.tsx）", () => {
  it("currentZoom：无 zoom 样式时回退 1", () => {
    expect(currentZoom()).toBe(1);
  });

  it("computeMainMinWidth：四档断点", () => {
    expect(computeMainMinWidth(1400)).toBe(580);
    expect(computeMainMinWidth(1000)).toBe(480);
    expect(computeMainMinWidth(800)).toBe(400);
    expect(computeMainMinWidth(500)).toBe(360);
  });

  it("computeLeftMin：三档断点", () => {
    expect(computeLeftMin(1200)).toBe(200);
    expect(computeLeftMin(900)).toBe(160);
    expect(computeLeftMin(500)).toBe(120);
  });
});
