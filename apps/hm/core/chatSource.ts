/**
 * 当前对话来源（阿黎 / 电脑 / 本机直聊）。
 *
 * 为什么放存储而不是路由参数：对话页是 **tabBar 页**，`uni.switchTab` 不接受
 * query。阿黎页把来源写进存储再切 tab，对话页在 `onShow` 里读回来——这是
 * tab 页之间传递上下文的唯一可靠通道（同 `hermix.restoreSession` 的既有做法）。
 */
import type { ChatSource } from './history'

const KEY = 'hermix.chatSource'

const VALID: ChatSource[] = ['ali', 'desktop', 'local']

/** 读当前来源。缺省 / 脏值 → 阿黎（手机的默认对话对象就是她）。 */
export function getChatSource(): ChatSource {
  try {
    const v = uni.getStorageSync(KEY)
    return VALID.includes(v) ? (v as ChatSource) : 'ali'
  } catch {
    return 'ali'
  }
}

export function setChatSource(source: ChatSource): void {
  try {
    uni.setStorageSync(KEY, source)
  } catch {
    // 写不进去也不该让页面挂掉：下一次进页面会回落到默认来源。
  }
}

/**
 * 来源的中文名与一句话说明。**描述必须与真实链路一致**——这里曾把阿黎写成
 * 「手机直接连云模型」，与本机直聊混为一谈，用户据此以为不连电脑也能和阿黎
 * 对话（2026-10-25 实报的误导源头）。
 */
export const SOURCE_META: Record<ChatSource, { name: string; desc: string; hint: string }> = {
  ali: {
    name: '阿黎',
    desc: '住在电脑上的助手 · 有记忆、能动手',
    hint: '她跑在你的电脑上：工具、工作区、记忆都在桌面端执行。电脑开机并连接后在这里说话。',
  },
  desktop: {
    name: '电脑',
    desc: '直接给电脑下任务 · 通用助手会话',
    hint: '不经过阿黎，直接遥控这台电脑干活：跑命令、改文件、调工具。电脑在线时可用。',
  },
  local: {
    name: '本机直聊',
    desc: '不连电脑 · 只用电脑推来的模型对话',
    hint: '用电脑此前推送的模型配置，在手机上直接和云端模型对话。不连接电脑、不能控制电脑、也没有阿黎——就是一段普通对话。',
  },
}
