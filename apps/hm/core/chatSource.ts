/**
 * 当前对话来源（阿黎 / 桌面）。
 *
 * 为什么放存储而不是路由参数：对话页是 **tabBar 页**，`uni.switchTab` 不接受
 * query。阿黎页把来源写进存储再切 tab，对话页在 `onShow` 里读回来——这是
 * tab 页之间传递上下文的唯一可靠通道（同 `hermix.restoreSession` 的既有做法）。
 */
import type { ChatSource } from './history'

const KEY = 'hermix.chatSource'

/** 读当前来源。缺省 / 脏值 → 桌面（用户最常用的那条路）。 */
export function getChatSource(): ChatSource {
  try {
    return uni.getStorageSync(KEY) === 'ali' ? 'ali' : 'desktop'
  } catch {
    return 'desktop'
  }
}

export function setChatSource(source: ChatSource): void {
  try {
    uni.setStorageSync(KEY, source)
  } catch {
    // 写不进去也不该让页面挂掉：下一次进页面会回落到默认来源。
  }
}

/** 来源的中文名与一句话说明（列表、顶栏、空态共用，避免三处各写一套）。 */
export const SOURCE_META: Record<ChatSource, { name: string; desc: string }> = {
  ali: {
    name: '阿黎',
    desc: '手机直接连云模型 · 电脑关机也能聊',
  },
  desktop: {
    name: '桌面',
    desc: '遥控那台电脑 · 经中转，由桌面端执行',
  },
}
