/**
 * 对话历史：本地存储。
 *
 * 数据模型：
 *   hermix.sessions     = SessionRec[]        （元数据清单，按 updatedAt 倒序）
 *   hermix.session.{id} = StoredBubble[]       （每条会话的消息体）
 *
 * SessionRec 只存元数据，不存消息体——这样列表页只需要读一个键，
 * 不会随消息增长而把 Storage 越撑越大。
 */

/** 对话气泡（历史存储用，与 chat/index.vue 的 Bubble 同构） */
export interface StoredBubble {
  id: string
  role: 'user' | 'assistant'
  text: string
  viaVoice?: boolean
  doneTools?: string[]
}

export interface SessionRec {
  id: string
  /** 标题 = 首条用户消息截前 40 字；未发过为「空白会话」 */
  title: string
  updatedAt: string  // ISO 字符串
  /** 消息总数（用户 + 助手），用来在列表里显示对话长度 */
  msgCount: number
}

const KEY_SESSIONS = 'hermix.sessions'
const keyFor = (id: string) => `hermix.session.${id}`

function sessions(): SessionRec[] {
  try {
    return JSON.parse(uni.getStorageSync(KEY_SESSIONS) || '[]')
  } catch {
    return []
  }
}

function saveSessions(list: SessionRec[]) {
  uni.setStorageSync(KEY_SESSIONS, JSON.stringify(list))
}

/** 生成会话 id */
export function newSessionId() {
  return `s${Date.now()}`
}

/** 截取标题 */
function makeTitle(messages: StoredBubble[]): string {
  const first = messages.find((m) => m.role === 'user')
  if (!first) return '空白会话'
  return first.text.slice(0, 40) + (first.text.length > 40 ? '…' : '')
}

/** 列出全部历史会话（倒序，最新的在前）。 */
export function listSessions(): SessionRec[] {
  return sessions().slice().sort((a, b) => b.updatedAt.localeCompare(a.updatedAt))
}

/** 读取某条会话的全部消息。 */
export function loadSession(id: string): StoredBubble[] {
  try {
    return JSON.parse(uni.getStorageSync(keyFor(id)) || '[]')
  } catch {
    return []
  }
}

/**
 * 保存（创建或更新）一条会话。
 * 创建时插入元数据，更新时只改 updatedAt + msgCount。
 */
export function saveSession(id: string, messages: StoredBubble[]) {
  const list = sessions()
  const existing = list.find((s) => s.id === id)
  const rec: SessionRec = {
    id,
    title: makeTitle(messages),
    updatedAt: new Date().toISOString(),
    msgCount: messages.length,
  }
  if (existing) {
    Object.assign(existing, rec)
  } else {
    list.unshift(rec)
  }
  saveSessions(list)
  uni.setStorageSync(keyFor(id), JSON.stringify(messages))
}

/**
 * 删除一条会话（同时删元数据和消息体）。
 */
export function deleteSession(id: string) {
  saveSessions(sessions().filter((s) => s.id !== id))
  uni.removeStorageSync(keyFor(id))
}

/** 根据 id 找到某条会话的标题（用于在列表页快速渲染，不读消息体）。 */
export function sessionTitle(id: string): string {
  return sessions().find((s) => s.id === id)?.title ?? '未知会话'
}
