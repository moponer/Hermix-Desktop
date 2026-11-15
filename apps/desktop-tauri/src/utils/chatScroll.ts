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
 * 距底部多少像素内仍算"贴底"。流式输出时内容逐帧长高，严格 `> 0`
 * 判定会在任意一帧把程序化滚动误判成"用户滚上去了"。
 */
export const AT_BOTTOM_TOLERANCE_PX = 24;

/**
 * 真实滚动输入（wheel / 触摸 / 键盘 / 拖动滚动条）的有效窗口。
 * 程序化跟随滚动不产生意图标记，因此不会被自己触发的 scroll 事件误判。
 */
export const USER_INTENT_WINDOW_MS = 400;

/**
 * 是否真的该把"离底"归因为用户上滚。
 *
 * 只有同时满足两点才成立：确实离开了底部容差范围，且窗口内有真实输入。
 * 缺第二个条件时（程序写完 scrollTop 后内容又长高）必须判 false ——
 * 否则粘性守卫会永久锁死自动跟随，只剩手动点"回到底部"一条出路。
 */
export function isUserScrolledUp(
  distanceFromBottom: number,
  hasRecentUserIntent: boolean,
): boolean {
  if (distanceFromBottom <= AT_BOTTOM_TOLERANCE_PX) return false;
  return hasRecentUserIntent;
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
