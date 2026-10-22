/**
 * 本地直连云模型（电脑离线时用）。不经过 relay，也不经过电脑。
 * 边界：只有纯对话（无工具/审批），走 OpenAI 兼容的 /chat/completions。
 */
import type { ModelConfig } from './model'
import { splitModelId } from './model'

export interface LocalTurn {
  role: 'user' | 'assistant'
  content: string
}

export interface LocalChatRequest {
  url: string
  header: Record<string, string>
  body: Record<string, unknown>
}

/** 组装请求（纯函数，可单测）。返回 { error } 而不是抛——这是用户能犯的错。 */
export function buildLocalRequest(
  config: ModelConfig,
  model: string,
  messages: LocalTurn[],
): LocalChatRequest | { error: string } {
  const parts = splitModelId(model)
  if (!parts) return { error: '模型名格式不对（应为 provider:model）' }
  const cred = (config.providers ?? []).find((p) => p.provider_id === parts.provider)
  if (!cred) return { error: `没有 ${parts.provider} 的凭据（电脑端可能还没推过配置）` }
  if (!cred.base_url) return { error: `${parts.provider} 没有配置 base_url` }
  const header: Record<string, string> = { 'Content-Type': 'application/json' }
  // 没有 key = 端点无需鉴权（不发这个头），与桌面侧 auth_header 一致。
  if (cred.api_key) header['Authorization'] = `Bearer ${cred.api_key}`
  return {
    url: `${cred.base_url.replace(/\/+$/, '')}/chat/completions`,
    header,
    body: { model: parts.model, messages, stream: true },
  }
}

export interface LocalChatHandlers {
  onDelta: (text: string) => void
  onDone: () => void
  onError: (message: string) => void
}

/** 发起一次流式对话。App 端分块收 SSE；H5 不支持 enableChunked，退化成一次性返回。 */
export function streamLocalChat(
  config: ModelConfig,
  model: string,
  messages: LocalTurn[],
  h: LocalChatHandlers,
): void {
  const req = buildLocalRequest(config, model, messages)
  if ('error' in req) {
    h.onError(req.error)
    return
  }
  const decoder = new TextDecoder('utf-8')
  let buf = ''
  let got = false
  let finished = false
  const finish = () => {
    if (finished) return
    finished = true
    h.onDone()
  }
  const fail = (msg: string) => {
    if (finished) return
    finished = true
    h.onError(msg)
  }
  /** 解析 SSE：`data: {...}` 一行一条，`[DONE]` 收尾。 */
  const consume = (text: string) => {
    buf += text
    let idx: number
    while ((idx = buf.indexOf('\n')) >= 0) {
      const line = buf.slice(0, idx).trim()
      buf = buf.slice(idx + 1)
      if (!line.startsWith('data:')) continue
      const payload = line.slice(5).trim()
      if (!payload) continue
      if (payload === '[DONE]') {
        finish()
        continue
      }
      try {
        const j = JSON.parse(payload)
        const d = j?.choices?.[0]?.delta?.content
        if (typeof d === 'string' && d) {
          got = true
          h.onDelta(d)
        }
      } catch {
        // 半截 JSON：等下一块。
      }
    }
  }
  const opts: any = {
    url: req.url,
    method: 'POST',
    header: req.header,
    data: req.body,
    timeout: 120000,
    enableChunked: true,
    success: (res: any) => {
      // 已经流式收到过增量 → success 里的完整 body 是同一份内容，别再念一遍。
      if (got) {
        finish()
        return
      }
      const data = res.data
      const text = data?.choices?.[0]?.message?.content
      if (typeof text === 'string' && text) {
        got = true
        h.onDelta(text)
      } else if (typeof data === 'string') {
        consume(data)
      }
      if (!got) {
        fail('云端没有返回内容')
        return
      }
      finish()
    },
    fail: (err: any) => {
      fail(String(err?.errMsg || '本地直连失败'))
    },
  }
  const task: any = uni.request(opts)
  if (typeof task?.onChunkReceived === 'function') {
    task.onChunkReceived((res: any) => {
      try {
        // stream:true → 解码器自己处理被切断的多字节字符。
        consume(decoder.decode(new Uint8Array(res.data), { stream: true }))
      } catch {
        // 坏块：跳过这一块，不打断整轮。
      }
    })
  }
}
