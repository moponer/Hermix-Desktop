/**
 * chatScroll unit tests — covers the pure helpers driving ChatView's
 * sticky-bottom + force-scroll behaviour.
 */
import { describe, it, expect } from "vitest";
import {
  shouldFollowOnStreamChunk,
  computeBottomScrollTop,
  distanceFromBottomPx,
  isUserScrolledUp,
  scrollToBottomWithSettle,
  AT_BOTTOM_TOLERANCE_PX,
} from "./chatScroll";

describe("shouldFollowOnStreamChunk", () => {
  it("follows when user is at the bottom and no force-scroll is set", () => {
    expect(shouldFollowOnStreamChunk(false, false)).toBe(true);
  });

  it("does NOT follow when user has scrolled up to browse history (sticky)", () => {
    expect(shouldFollowOnStreamChunk(true, false)).toBe(false);
  });

  it("force-scroll overrides sticky-bottom guard (user just submitted)", () => {
    expect(shouldFollowOnStreamChunk(true, true)).toBe(true);
    expect(shouldFollowOnStreamChunk(false, true)).toBe(true);
  });
});

describe("computeBottomScrollTop", () => {
  it("returns scrollHeight - clientHeight for a scrollable container", () => {
    expect(computeBottomScrollTop(2_000, 600)).toBe(1_400);
  });

  it("returns 0 when content fits within viewport", () => {
    expect(computeBottomScrollTop(400, 600)).toBe(0);
  });

  it("clamps to 0 when scrollHeight < clientHeight (transient reflow)", () => {
    expect(computeBottomScrollTop(500, 600)).toBe(0);
  });

  it("handles zero scrollHeight (empty container)", () => {
    expect(computeBottomScrollTop(0, 600)).toBe(0);
  });
});

describe("distanceFromBottomPx", () => {
  it("is zero when scrolled to the bottom", () => {
    expect(distanceFromBottomPx(1_400, 2_000, 600)).toBe(0);
  });

  it("is positive when scrolled above the bottom", () => {
    expect(distanceFromBottomPx(0, 2_000, 600)).toBe(1_400);
    expect(distanceFromBottomPx(500, 2_000, 600)).toBe(900);
  });

  it("is negative when the viewport extends past the bottom (large viewport)", () => {
    expect(distanceFromBottomPx(0, 400, 600)).toBe(-200);
  });
});

// Test helpers — see scrollToBottomWithSettle JSDoc for why this exists.
// jsdom doesn't lay out content, so we mock scrollHeight/clientHeight
// via getters that the test can update between frames.
function makeMockEl(): {
  el: HTMLElement;
  setHeight: (h: number) => void;
  setClient: (h: number) => void;
  top: number;
} {
  let scrollHeight = 1000;
  let clientHeight = 600;
  const el = document.createElement("div");
  Object.defineProperty(el, "scrollHeight", {
    configurable: true,
    get: () => scrollHeight,
  });
  Object.defineProperty(el, "clientHeight", {
    configurable: true,
    get: () => clientHeight,
  });
  return {
    el,
    setHeight: (h) => {
      scrollHeight = h;
    },
    setClient: (h) => {
      clientHeight = h;
    },
    get top() {
      return el.scrollTop;
    },
  };
}

// Flush rAFs: schedules `frames` requestAnimationFrame ticks. Returns when
// all of them have fired (or after a generous timeout for slow CI).
async function flushFrames(frames: number): Promise<void> {
  for (let i = 0; i < frames; i++) {
    await new Promise<void>((r) => requestAnimationFrame(() => r()));
  }
}

// 回归守卫：流式输出时"自动滚动突然停住、必须手动点回到底部"。
// 根因是程序写完 scrollTop 后内容又长高，同一 scroll 事件里 dist > 0，
// 被旧逻辑当成用户上滚。没有用户意图就必须判 false，否则粘性守卫
// 一旦置 true 就再也复位不了（不再有 scroll 事件）。
describe("isUserScrolledUp", () => {
  it("does NOT treat programmatic drift as user scroll (streaming grew after we pinned bottom)", () => {
    expect(isUserScrolledUp(30, false)).toBe(false);
    expect(isUserScrolledUp(5_000, false)).toBe(false);
  });

  it("treats a real wheel/touch scroll away from bottom as user scroll", () => {
    expect(isUserScrolledUp(200, true)).toBe(true);
  });

  it("tolerates sub-threshold jitter while pinned to the bottom", () => {
    expect(isUserScrolledUp(AT_BOTTOM_TOLERANCE_PX, true)).toBe(false);
    expect(isUserScrolledUp(AT_BOTTOM_TOLERANCE_PX + 1, true)).toBe(true);
  });

  it("releases the sticky guard when the viewport is back at the bottom", () => {
    expect(isUserScrolledUp(0, true)).toBe(false);
    expect(isUserScrolledUp(-12, true)).toBe(false);
  });
});

describe("scrollToBottomWithSettle", () => {
  it("one-shot scroll lands at bottom when scrollHeight is stable", async () => {
    const m = makeMockEl();
    m.setHeight(2000);
    scrollToBottomWithSettle(m.el, 15);
    await flushFrames(4);
    expect(m.top).toBe(2000 - 600);
  });

  it("keeps re-applying scrollTop while scrollHeight is still growing", async () => {
    const m = makeMockEl();
    m.setHeight(1000);
    scrollToBottomWithSettle(m.el, 15);

    // Simulate messages loading in chunks across frames.
    m.setHeight(1800);
    await flushFrames(1);
    m.setHeight(2400);
    await flushFrames(1);
    m.setHeight(3000);
    await flushFrames(1);

    // After growth stops, final scrollTop should match the latest height.
    expect(m.top).toBe(3000 - 600);
  });

  it("stops after maxFrames even when scrollHeight keeps changing", async () => {
    const m = makeMockEl();
    m.setHeight(1000);
    scrollToBottomWithSettle(m.el, 3);

    // Grow on every frame; with cap=3 we should stop early.
    for (let i = 0; i < 8; i++) {
      m.setHeight(1000 + (i + 1) * 1000);
      await flushFrames(1);
    }
    await flushFrames(2);
    // Final scrollTop should reflect some early height, not the final
    // one — otherwise the retry didn't respect the cap.
    const finalHeight = 1000 + 9 * 1000;
    expect(m.top).toBeLessThan(finalHeight - 600);
  });

  it("cancel() returned by the helper stops the retry loop", async () => {
    const m = makeMockEl();
    m.setHeight(1000);
    const cancel = scrollToBottomWithSettle(m.el, 15);
    await flushFrames(3);
    const captured = m.top;
    cancel();
    m.setHeight(10000);
    await flushFrames(5);
    // After cancel, no more re-scrolls should happen.
    expect(m.top).toBe(captured);
  });

  it("isCancelled callback aborts retry when user scrolls up mid-restore", async () => {
    const m = makeMockEl();
    m.setHeight(1000);
    let aborted = false;
    scrollToBottomWithSettle(m.el, 15, () => aborted);
    await flushFrames(3);
    aborted = true;
    const captured = m.top;
    m.setHeight(10000);
    await flushFrames(5);
    expect(m.top).toBe(captured);
  });

  it("maxFrames=0 skips retries — single scroll, no follow-up", async () => {
    const m = makeMockEl();
    m.setHeight(2000);
    scrollToBottomWithSettle(m.el, 0);
    await flushFrames(4);
    const captured = m.top;
    m.setHeight(8000);
    await flushFrames(4);
    expect(m.top).toBe(captured);
    expect(captured).toBe(2000 - 600);
  });
});