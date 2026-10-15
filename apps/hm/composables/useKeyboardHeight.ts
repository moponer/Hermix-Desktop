import { ref } from 'vue'

/**
 * 键盘高度补偿。
 *
 * 返回当前键盘遮挡高度（px），并把它写进 CSS 变量 `--kb-height`，供页面样式
 * 做位移。只有存在 `VisualViewport` 的环境（H5）才生效；App-vue / 小程序没有
 * `window.visualViewport`，此时恒为 0，页面按 0 处理即可。
 */
export function useKeyboardHeight() {
  const kbHeight = ref(0)
  if (typeof window === 'undefined') return { kbHeight }
  const vv = window.visualViewport
  if (!vv) return { kbHeight }

  let lastKb = 0
  let rafId = 0
  const raf = window.requestAnimationFrame || ((fn: () => void) => setTimeout(fn, 16))
  const compute = () => Math.max(0, Math.round(window.innerHeight - vv.height - vv.offsetTop))
  const apply = () => {
    const kb = compute()
    // 阈值去抖：小抖动不重排，避免输入时页面跟着颤。
    if (Math.abs(kb - lastKb) < 6) return
    lastKb = kb
    kbHeight.value = kb
    document.documentElement.style.setProperty('--kb-height', kb + 'px')
  }
  const schedule = () => {
    if (rafId) return
    rafId = raf(() => {
      rafId = 0
      apply()
    })
  }

  vv.addEventListener('resize', schedule)
  vv.addEventListener('scroll', schedule)
  window.addEventListener('resize', schedule)
  window.addEventListener('orientationchange', () => setTimeout(schedule, 200))
  setTimeout(schedule, 100)
  return { kbHeight }
}
