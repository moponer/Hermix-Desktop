/**
 * 对话历史：本地存储。
 *
 * 数据模型：
 *   hermix.sessions     = SessionRec[]        （元数据清单，按 updatedAt 倒序）
 *   hermix.session.{id} = StoredBubble[]       （每条会话的消息体）
 *
 * SessionRec 只存元数据，不存消息体——这样列表页只需要读一个键，
 * 不会随消息增长而把 Storage 越撑越大。
 *
 * ## 为什么必须带 source
 *
 * 手机上有两条**完全不同**的对话通路：
 * - `ali`：阿黎，直接用电脑推来的凭据连云厂商（电脑关机也能聊）；
 * - `desktop`：遥控那台电脑，经 relay 由桌面端的 agent 跑。
 *
 * 两条通路的会话混在一个列表里，用户就分不清"这句话是谁答的、在哪能找回"。
 * 所以来源是**数据结构的一部分**，不是渲染时的视觉标签——列表、恢复、删除
 * 都按它分流。
 */

/** 对话来源：阿黎（手机本地直连）/ 桌面（遥控那台电脑）。 */
export type ChatSource = 'ali' | 'desktop'

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
  /** 来源。旧数据没有这个字段 → 按 `desktop` 处理（见 normalize）。 */
  source: ChatSource
  /** 标题 = 首条用户消息截前 40 字；未发过为「空白会话」 */
  title: string
  updatedAt: string // ISO 字符串
  /** 消息总数（用户 + 助手），用来在列表里显示对话长度 */
  msgCount: number
}

const KEY_SESSIONS = 'hermix.sessions'
const keyFor = (id: string) => `hermix.session.${id}`

/**
 * 旧记录的兼容归一。
 *
 * 这个仓库里已经存在（用户手机上已经存着）没有 `source` 的历史会话——
 * 它们全部来自"遥控电脑"那条路（阿黎是后加的）。默认成 `desktop` 而不是
 * 丢弃：迁移不是删数据的理由。
 */
function normalize(rec: Partial<SessionRec> & { id: string }): SessionRec {
  return {
    id: rec.id,
    source: rec.source === 'ali' ? 'ali' : 'desktop',
    title: rec.title || '空白会话',
    updatedAt: rec.updatedAt || new Date(0).toISOString(),
    msgCount: typeof rec.msgCount === 'number' ? rec.msgCount : 0,
  }
}

function sessions(): SessionRec[] {
  try {
    const raw = JSON.parse(uni.getStorageSync(KEY_SESSIONS) || '[]')
    if (!Array.isArray(raw)) return []
    return raw
      .filter((r: any) => r && typeof r.id === 'string')
      .map((r: any) => normalize(r))
  } catch {
    return []
  }
}

function saveSessions(list: SessionRec[]) {
  uni.setStorageSync(KEY_SESSIONS, JSON.stringify(list))
}

/** 生成会话 id（按来源分前缀，纯为排障时一眼看出归属）。 */
export function newSessionId(source: ChatSource = 'desktop') {
  return `${source === 'ali' ? 'a' : 's'}${Date.now()}`
}

/** 截取标题 */
function makeTitle(messages: StoredBubble[]): string {
  const first = messages.find((m) => m.role === 'user')
  if (!first) return '空白会话'
  return first.text.slice(0, 40) + (first.text.length > 40 ? '…' : '')
}

/**
 * 列出历史会话（倒序，最新的在前）。
 * 传 `source` 则只列该来源——阿黎页与桌面历史各自只看自己那一半。
 */
export function listSessions(source?: ChatSource): SessionRec[] {
  const all = sessions()
    .filter((s) => (source ? s.source === source : true))
    .sort((a, b) => b.updatedAt.localeCompare(a.updatedAt))
  return all
}

/** 读取某条会话的全部消息。 */
export function loadSession(id: string): StoredBubble[] {
  try {
    return JSON.parse(uni.getStorageSync(keyFor(id)) || '[]')
  } catch {
    return []
  }
}

/** 某条会话的来源（读不到按 desktop 处理，与 normalize 一致）。 */
export function sessionSource(id: string): ChatSource {
  return sessions().find((s) => s.id === id)?.source ?? 'desktop'
}

/**
 * 保存（创建或更新）一条会话。
 * 创建时插入元数据，更新时只改 updatedAt + msgCount。
 *
 * 更新**不改来源**：一条会话一旦创建就属于它诞生的那条通路，中途被改来源
 * 会让历史列表把它挪到另一边，用户会以为"对话丢了"。
 */
export function saveSession(id: string, messages: StoredBubble[], source: ChatSource = 'desktop') {
  const list = sessions()
  const existing = list.find((s) => s.id === id)
  const rec: SessionRec = {
    id,
    source: existing?.source ?? source,
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
