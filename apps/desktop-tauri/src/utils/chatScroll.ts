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

/**
 * Scroll `el` to its bottom and keep re-applying until `scrollHeight`
 * stops growing (or we hit `maxFrames`).
 *
 * Why this exists: when restoring a long conversation, the chat container
 * mounts with `messagesEndRef` already inside the viewport, so a single
 * `scrollIntoView({ block: "end" })` is a no-op. Round 31 (commit
 * b02923e) replaced it with `scrollTop = scrollHeight - clientHeight` and
 * wrapped that in a double-rAF to wait for React commit + browser layout.
 *
 * In practice double-rAF is not enough: rich content (markdown, images,
 * web fonts, syntax-highlighted code blocks) keeps the container's
 * `scrollHeight` growing for many frames after the initial commit. Setting
 * `scrollTop` too early lands it on what was the bottom at +2 frames,
 * not what is the bottom after all the layout settles - owner reported
 * "some restored conversations don't auto-scroll, have to scroll down
 * manually". The fix is to keep re-applying the scroll until the
 * measured `scrollHeight` matches the previous frame's reading (i.e.
 * the container has stopped growing).
 *
 * Behaviour:
 *   - First call: schedules the scroll via `requestAnimationFrame` so the
 *     caller doesn't have to. Re-applies on every subsequent rAF until
 *     `scrollHeight` is stable across two consecutive frames.
 *   - Returns a cancel function. Calling it aborts the retry loop (used
 *     when the user scrolls away mid-restore - we shouldn't keep
 *     yanking them back).
 *   - For the "force once" path (handleSubmit) pass `maxFrames: 0` to
 *     skip the retry entirely.
 *
 * @param el           Scrollable container element
 * @param maxFrames    Cap on retry frames (default 15 ~ 250ms @ 60fps).
 *                     Set to 0 for a one-shot scroll.
 * @param isCancelled  Optional callback returning true if the caller has
 *                     aborted (e.g. user scrolled up). Retry stops here.
 * @returns            `() => void` cancel handle.
 */
export function scrollToBottomWithSettle(
  el: HTMLElement,
  maxFrames: number = 15,
  isCancelled?: () => boolean,
): () => void {
  let cancelled = false;
  let attempts = 0;
  let lastHeight = -1;
  let rafId = 0;

  const cancel = () => {
    cancelled = true;
    if (rafId) cancelAnimationFrame(rafId);
  };

  const tick = () => {
    if (cancelled || (isCancelled && isCancelled())) return;
    el.scrollTop = computeBottomScrollTop(el.scrollHeight, el.clientHeight);
    const height = el.scrollHeight;
    attempts++;
    // Stop on stable scrollHeight (matches last frame) or cap reached.
    // The exact-equality check tolerates sub-pixel layout jitter via the
    // `height === lastHeight` check - a 1px shift still counts as
    // "growing" and triggers one more retry. With typical content the
    // loop terminates within 3-6 frames; 15 is a safety cap.
    if (height === lastHeight || attempts >= maxFrames) return;
    lastHeight = height;
    rafId = requestAnimationFrame(tick);
  };

  // Two-frame warmup lets React commit + browser finish initial layout.
  rafId = requestAnimationFrame(() => {
    rafId = requestAnimationFrame(tick);
  });

  return cancel;
}
