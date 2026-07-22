/**
 * chatScroll helpers — pure functions driving ChatView's sticky-bottom +
 * force-scroll behaviour.
 *
 * Background (2026-07-22, owner-reported bug): the previous implementation
 * used `messagesEndRef.scrollIntoView({ block: "end" })` for both streaming
 * and post-submit scrolls. Two failure modes:
 *
 *   1. `scrollIntoView` is a no-op when the target is already inside the
 *      scrollable viewport. Streaming output that *grows* the container
 *      above the visible area (e.g. an inline tool timeline row expanding
 *      while `messagesEndRef` stays at the bottom of the existing viewport)
 *      therefore leaves `scrollTop` unmoved — the user has to scroll down
 *      manually to see the new chunk. Bug reproduction: long tool timeline
 *      expansion mid-stream.
 *
 *   2. Submitting a new message while the user has scrolled up to browse
 *      history respects the `hasUserScrolledUp` sticky-bottom guard, so the
 *      viewport stays where it was. The user expects to see the message
 *      they just sent.
 *
 * The fix replaces `scrollIntoView` with an explicit `scrollTop = scrollHeight
 * - clientHeight` write, and adds a `forceScrollSeq` signal so user-initiated
 * events (handleSubmit) bypass the sticky guard.
 */

/**
 * Whether a scroll request should actually move the viewport to the bottom.
 *
 * `forceScroll` overrides everything — caller passed a user-initiated event
 * (handleSubmit / session restore) that must always land the user at the
 * latest message. `hasUserScrolledUp` is the sticky guard: when the user
 * has scrolled away from the bottom to browse history, streaming chunks
 * must NOT yank them back.
 */
export function shouldFollowOnStreamChunk(
  hasUserScrolledUp: boolean,
  forceScroll: boolean,
): boolean {
  if (forceScroll) return true;
  if (hasUserScrolledUp) return false;
  return true;
}

/**
 * Compute the `scrollTop` value that lands the viewport at the bottom of
 * the container's content.
 *
 * Replaces `messagesEndRef.scrollIntoView({ block: "end" })` because the
 * latter is a no-op when the target node is already inside the viewport —
 * which is the common case during streaming once the container has grown
 * past the initial viewport size.
 *
 * Negative values are clamped to 0 — some browsers report a transient
 * `scrollHeight < clientHeight` during reflow.
 */
export function computeBottomScrollTop(
  scrollHeight: number,
  clientHeight: number,
): number {
  const target = scrollHeight - clientHeight;
  return target > 0 ? target : 0;
}

/**
 * Distance from the bottom in pixels. Positive when the viewport is
 * above the bottom (i.e. the user has scrolled up); zero or negative
 * when the bottom is at or below the viewport edge.
 *
 * Mirrors the inline expression used by `isNearBottom` in ChatView.
 */
export function distanceFromBottomPx(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
): number {
  return scrollHeight - scrollTop - clientHeight;
}