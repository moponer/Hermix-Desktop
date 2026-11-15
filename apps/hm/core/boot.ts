/**
 * 致命错误的落点。
 *
 * ## 为什么需要它
 *
 * 改造前的启动路径是：splashscreen（`waiting:true` + `alwaysShowBeforeRender:true`）
 * → 首个页面渲染 → splash 自动关闭。**中间没有任何一环会失败**——`main.js`
 * 当时只有 6 行，没有 errorHandler，`App.vue` 没有 onError。于是任何一个
 * setup 异常的后果都是：启动图永远不关（转圈）→ 系统超时放弃 → webview
 * 变空白 → splash 的 waiting 层残留成一层半透明黑蒙版。用户看到的是
 * 「转圈 → 空白 → 一块黑色透明蒙版」，**拿不到任何一行错误信息**。
 *
 * 现在反过来：异常必须**立刻变得可见且可恢复**。
 * - 内存里存一份，供 boot 页渲染；
 * - storage 里存一份，跨启动存活——因为「boot 页自己也崩了」是唯一
 *   还能兜住这种情况的地方。
 */
import { ref } from 'vue'

const KEY = 'hermix.fatal'

export interface Fatal {
  /** 一句话结论 */
  message: string
  /** 原始错误文本 */
  detail: string
  /** Vue 的 info 串（哪个生命周期抛的） */
  info: string
  at: number
}

function readStored(): Fatal | null {
  try {
    return (uni.getStorageSync(KEY) as Fatal) || null
  } catch {
    return null
  }
}

/** 最近的致命错误；null = 没有。boot 页读它决定渲染错误屏还是继续分流。 */
export const fatal = ref<Fatal | null>(readStored())

function describe(e: unknown): string {
  if (e instanceof Error) return e.message || e.name
  if (typeof e === 'string') return e
  try {
    return JSON.stringify(e)
  } catch {
    return String(e)
  }
}

/** 记一次致命错误。**幂等**：连续报多次只留第一条，避免后来者覆盖根因。 */
export function reportFatal(e: unknown, info = ''): void {
  if (fatal.value) return
  const rec: Fatal = { message: describe(e), detail: String(e), info, at: Date.now() }
  fatal.value = rec
  try {
    uni.setStorageSync(KEY, rec)
  } catch {
    // storage 写不进去也不能让上报本身再抛一次——那是二次崩溃。
  }
}

/** 清掉。用户点「重试」时调。 */
export function clearFatal(): void {
  fatal.value = null
  try {
    uni.removeStorageSync(KEY)
  } catch {
    /* 同上 */
  }
}

/**
 * 显式关闭启动图。
 *
 * ## 为什么必须手动关
 *
 * 源 manifest 写 `alwaysShowBeforeRender:false` 后，编译器（uni-cli-shared
 * splashscreen.js 的 initSplashscreen）会把打进 APK 的原生 autoclose 改成
 * **false**，注释称「启用 uni-app 框架关闭 splash」——但当前基座的
 * uni-jsframework-vue3.js 里没有任何关闭逻辑（全文件 0 处 splash 字样），
 * 结果就是启动图永远不关，新 UI 全画在它后面，用户看到「一直转圈」。
 * 只能由页面渲染完成后自己关。幂等：原生重复关无副作用。
 */
export function ensureSplashClosed(): void {
  // #ifdef APP-PLUS
  try {
    ;(globalThis as unknown as { plus?: { navigator?: { closeSplashscreen?: () => void } } })
      .plus?.navigator?.closeSplashscreen?.()
  } catch {
    /* plus 未就绪：尚无启动图可关，由后续调用点兜底 */
  }
  // #endif
}
