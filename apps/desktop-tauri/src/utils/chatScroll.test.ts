/**
 * chatScroll unit tests — covers the 3 pure helpers driving ChatView's
 * sticky-bottom + force-scroll behaviour.
 */
import { describe, it, expect } from "vitest";
import {
  shouldFollowOnStreamChunk,
  computeBottomScrollTop,
  distanceFromBottomPx,
} from "./chatScroll";

describe("shouldFollowOnStreamChunk", () => {
  it("follows when user is at the bottom and no force-scroll is set", () => {
    expect(shouldFollowOnStreamChunk(false, false)).toBe(true);
  });

  it("does NOT follow when user has scrolled up to browse history (sticky)", () => {
    // The "内容会在某个位置停住" bug from the owner report had the wrong
    // polarity here — the streaming rAF used to call scrollIntoView but
    // the messagesEndRef stayed inside the viewport and the browser didn't
    // move scrollTop. The fix doesn't change this guard (sticky is correct
    // when the user is actively browsing history), it changes the
    // *scroll mechanism* so the *follow* branch actually scrolls.
    expect(shouldFollowOnStreamChunk(true, false)).toBe(false);
  });

  it("force-scroll overrides sticky-bottom guard (user just submitted)", () => {
    // Owner report: "往上翻看对话内容后...发送消息出去不会自动滚动到
    // 用户消息的这个位置". Submit is user-initiated; must land the user
    // on the message they just sent regardless of prior scroll position.
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
    // Some browsers report scrollHeight below clientHeight during initial
    // reflow; we'd otherwise set a negative scrollTop which the browser
    // silently rounds to 0 anyway — but the explicit clamp is cheaper
    // than letting it become NaN.
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
    // Only happens with oversized clientHeight; should be treated as
    // "at or past the bottom" by sticky-bottom logic.
    expect(distanceFromBottomPx(0, 400, 600)).toBe(-200);
  });
});