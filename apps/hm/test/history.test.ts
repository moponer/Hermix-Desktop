/**
 * 三来源会话历史的分组行为。来源是数据结构的一部分：阿黎（电脑上的人格）、
 * 电脑（设备通用会话）、本机直聊（不连电脑）——三条通路的会话混列会让用户
 * 分不清「这句话是谁答的」。这里把分组、归一、前缀、来源不变性钉死。
 */
import test from 'node:test'
import assert from 'node:assert/strict'
import {
  newSessionId,
  saveSession,
  loadSession,
  listSessions,
  sessionSource,
  deleteSession,
} from '../core/history'
import { getChatSource, setChatSource, SOURCE_META } from '../core/chatSource'
import { saveModelConfig, loadModelConfig, setLocalDefaultModel } from '../core/modelStore'

// 内存版 uni storage：三个被测模块都只在函数体内触 uni，模块加载期不碰——
// 静态 import 先执行也无妨，只要这个 mock 在测试体运行前装好（模块顶层语句保证）。
const store = new Map<string, unknown>()
;(globalThis as Record<string, unknown>).uni = {
  getStorageSync: (k: string) => (store.has(k) ? store.get(k) : ''),
  setStorageSync: (k: string, v: unknown) => {
    store.set(k, v)
  },
  removeStorageSync: (k: string) => {
    store.delete(k)
  },
}

test('三个来源各自成列，互不混入', () => {
  saveSession(newSessionId('ali'), [{ id: '1', role: 'user', text: '阿黎你好' }], 'ali')
  saveSession(newSessionId('desktop'), [{ id: '2', role: 'user', text: '跑个命令' }], 'desktop')
  saveSession(newSessionId('local'), [{ id: '3', role: 'user', text: '随便聊聊' }], 'local')

  const ali = listSessions('ali')
  const desk = listSessions('desktop')
  const local = listSessions('local')

  assert.equal(ali.length, 1)
  assert.equal(desk.length, 1)
  assert.equal(local.length, 1)
  assert.equal(ali[0].source, 'ali')
  assert.equal(desk[0].source, 'desktop')
  assert.equal(local[0].source, 'local')
})

test('会话 id 按来源分前缀（a/l/s），排障时一眼看出归属', () => {
  assert.equal(newSessionId('ali')[0], 'a')
  assert.equal(newSessionId('local')[0], 'l')
  assert.equal(newSessionId('desktop')[0], 's')
})

test('旧记录没有 source 时归入 desktop，local 不被吞', () => {
  // 直接塞一条没有 source 的旧数据（早于来源字代的记录）
  store.set(
    'hermix.sessions',
    JSON.stringify([{ id: 'legacy-1', title: '旧会话', updatedAt: '2026-01-01T00:00:00Z', msgCount: 2 }]),
  )
  assert.equal(sessionSource('legacy-1'), 'desktop')
  assert.equal(listSessions('desktop').some((s) => s.id === 'legacy-1'), true)
  // 显式 local 的记录必须保得住（归一白名单里有它）
  store.set(
    'hermix.sessions',
    JSON.stringify([
      { id: 'legacy-1', source: 'local', title: 'x', updatedAt: '2026-01-01T00:00:00Z', msgCount: 1 },
    ]),
  )
  assert.equal(sessionSource('legacy-1'), 'local')
})

test('会话创建后更新不再改来源——中途换来源会让用户以为对话丢了', () => {
  const id = newSessionId('local')
  saveSession(id, [{ id: '1', role: 'user', text: 'hi' }], 'local')
  saveSession(id, [{ id: '1', role: 'user', text: 'hi' }, { id: '2', role: 'assistant', text: 'yo' }], 'ali')
  assert.equal(sessionSource(id), 'local')
})

test('删除只删那一条，别的来源不受影响', () => {
  const a = newSessionId('ali')
  const l = newSessionId('local')
  saveSession(a, [{ id: '1', role: 'user', text: 'a' }], 'ali')
  saveSession(l, [{ id: '1', role: 'user', text: 'l' }], 'local')
  deleteSession(a)
  assert.equal(listSessions('ali').length, 0)
  assert.equal(loadSession(l).length, 1)
})

test('chatSource：缺省是阿黎（手机的默认对象），脏值回落而不是抛', () => {
  store.delete('hermix.chatSource')
  assert.equal(getChatSource(), 'ali')
  store.set('hermix.chatSource', 'alien')
  assert.equal(getChatSource(), 'ali')
  setChatSource('local')
  assert.equal(getChatSource(), 'local')
})

test('SOURCE_META 三条通路的描述都写明了链路（防再出现“阿黎=直连云模型”式误导）', () => {
  assert.equal(Object.keys(SOURCE_META).length, 3)
  // 阿黎必须说她在电脑上执行
  assert.match(SOURCE_META.ali.desc, /电脑/)
  // 本机直聊必须声明不连电脑
  assert.match(SOURCE_META.local.desc, /不连电脑/)
})

test('modelStore：本机默认模型只改本地快照，推送来了会被覆盖', () => {
  saveModelConfig({
    models: [
      { provider_id: 'p', model_id: 'a' },
      { provider_id: 'p', model_id: 'b' },
    ],
    providers: [{ provider_id: 'p', base_url: 'https://x', api_key: 'k', api_style: 'openai' }],
    defaultModel: 'p:a',
  })
  setLocalDefaultModel('p:b')
  assert.equal(loadModelConfig()?.defaultModel, 'p:b')
  // 电脑重推整份 → 本地选择让位给真相源
  saveModelConfig({
    models: [{ provider_id: 'p', model_id: 'c' }],
    providers: [],
    defaultModel: 'p:c',
  })
  assert.equal(loadModelConfig()?.defaultModel, 'p:c')
})
