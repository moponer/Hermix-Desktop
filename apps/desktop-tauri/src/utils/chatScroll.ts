// Pure functions driving ChatView's sticky-bottom + force-scroll
// behaviour. We deliberately don't use `Element.scrollIntoView` — it
// is a no-op when the target is already inside the viewport, so it
// can't pin the bottom during streaming when the container is
// growing, and it can't help during session restore when rich
// content (markdown, images, web fonts) keeps `scrollHeight` moving
// for many frames after the initial commit. An explicit
// `scrollTop = scrollHeight - clientHeight` write, re-applied until
// the height is stable, handles both cases.

/**
 * Whether a scroll request should actually move the viewport to the bottom.
 * `forceScroll` overrides everything (user-initiated submit / session
 * restore must always land at the latest message). `hasUserScrolledUp`
 * is the sticky guard: when the user has scrolled away to browse
 * history, streaming chunks must NOT yank them back.
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
 * `scrollTop` that lands the viewport at the bottom of the container's
 * content. Clamped to 0 because some browsers report a transient
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
 * above the bottom (user has scrolled up); zero or negative when the
 * bottom is at or below the viewport edge.
 */
export function distanceFromBottomPx(
  scrollTop: number,
  scrollHeight: number,
  clientHeight: number,
): number {
  return scrollHeight - scrollTop - clientHeight;
}

/**
 * Scrolls `el` to its bottom and re-applies the scroll each animation
 * frame until `scrollHeight` is stable across two consecutive reads
 * (or `maxFrames` is reached). Rich content (markdown, images, web
 * fonts, syntax-highlighted code blocks) keeps `scrollHeight` growing
 * for many frames after the initial commit; a single rAF settles too
 * early and lands on what *was* the bottom, not what is the bottom
 * after layout stabilises.
 *
 * Behaviour:
 *   - First call schedules the scroll via `requestAnimationFrame`.
 *   - Returns a cancel handle. Calling it aborts the retry loop (use
 *     when the user scrolls away mid-restore — don't yank them back).
 *   - Pass `maxFrames: 0` for a one-shot scroll (the "force once"
 *     handleSubmit path).
 *
 * @param el           Scrollable container element.
 * @param maxFrames    Retry cap (default 15 ≈ 250ms @ 60fps). 0 = one-shot.
 * @param isCancelled  Optional callback returning true if the caller
 *                     has aborted (e.g. user scrolled up). Retry stops.
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
    // Stable across two consecutive reads = layout has settled.
    // Exact-equality tolerates sub-pixel jitter (a 1px shift still
    // counts as growing and triggers one more retry). Typical
    // content settles in 3-6 frames; the cap is a safety belt.
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
