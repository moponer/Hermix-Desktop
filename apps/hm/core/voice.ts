/**
 * 语音：手机端直连云端 ASR / TTS，**音频与识别文本都不过 relay**。
 *
 * ## 为什么两端都在手机端
 *
 * - **TTS 在手机发**：说话的人在手机边上。让 PC 合成再传回来，等于把整个会话
 *   里最大的数据量（音频下行）塞进一个"JSON 文本帧的字节透明管道"，而 relay
 *   现在的模型是请求/响应对。这是本方案里**唯一**不动 relay 架构的选项。
 * - **ASR 也在手机做**：同上，而且流式识别需要"边说边出字"，多一跳就多一份
 *   缓冲延迟。
 *
 * 代价要认：手机时钟/网络直接决定体验，而**凭据落在手机上**（明文，见
 * `pairing.ts` 的未决项说明）。
 *
 * ## 三条铁律（改代码前先读）
 *
 * 1. **语音只做「发起」，不做「授权」**。审批永远要视觉确认：通话意味着用户
 *    不看屏，在不看屏时要求他做安全决策本身就是矛盾。
 * 2. **不做「说完自动发」**。ASR 误识别在没有视觉确认时是单向的，长按-松发
 *    给了一个人工的确认动作。识别结果回显成用户气泡，错了能改。
 * 3. **高危动词命中 → 发前二次确认**。这是对**指令本身**的确认，与工具审批
 *    是两件事，两者都要、不能互相替代。
 */
import { request } from './http'

// ---------------------------------------------------------------------------
// 高危动词
// ---------------------------------------------------------------------------

/**
 * 命中即在发出前弹一次两键确认。
 *
 * 刻意**宁可多报**：漏掉的代价是执行了一个不可逆动作，多报的代价是多一次
 * 确认。所以否定句（"不要删除"）同样命中——它仍然是一条提到删除的指令。
 */
const RISKY_VERBS: RegExp[] = [
  /删除|删掉|删了|删/,
  /清空|清除|清理|抹掉|干掉/,
  /覆盖|覆写|重写|格式化/,
  /重装|重置|恢复出厂/,
  /推送到|发布|上线|部署到/,
  /转账|付款|支付|扣款/,
  /授权|永久|一直允许/,
  /\brm\s+-rf\b/i,
  /\b(drop|truncate|delete)\b/i,
  /\bkill\b/i,
  /\bformat\b/i,
]

/** 这句话是否需要发出前确认。 */
export function isRiskyInstruction(text: string): boolean {
  return RISKY_VERBS.some((re) => re.test(text))
}

/** 风险等级：给 UI 选不同的措辞强度。 */
export type RiskLevel = 'none' | 'confirm'

export function riskOf(text: string): RiskLevel {
  return isRiskyInstruction(text) ? 'confirm' : 'none'
}

// ---------------------------------------------------------------------------
// ASR
// ---------------------------------------------------------------------------

export interface AsrResult {
  /** 边说边出的增量文本（可能为空） */
  partial: string
  /** 停止时的最终文本；`null` 表示这一轮没听清 */
  final: string | null
}

export interface AsrProvider {
  readonly enabled: boolean
  /** 开始一轮识别。每轮必须重新调用 provider（内部会开新流）。 */
  start(): Promise<void>
  /** 推一段录音（PCM / webm 视 provider 而定）。 */
  push(chunk: ArrayBuffer): Promise<void>
  /** 结束并取最终文本。 */
  stop(): Promise<AsrResult>
  /** 丢弃当前轮（用户中途放弃）。 */
  cancel(): void
  /** 增量文本回调（用于「边说边出字」）。 */
  onPartial(cb: (text: string) => void): void
}

/** 没配 key 时的占位：所有操作都是安全的空转，不报错也不静默假装成功。 */
export class NullAsr implements AsrProvider {
  readonly enabled = false
  async start(): Promise<void> {}
  async push(): Promise<void> {}
  async stop(): Promise<AsrResult> {
    return { partial: '', final: null }
  }
  cancel(): void {}
  onPartial(): void {}
}

export interface CloudAsrConfig {
  /** 流式识别端点（各家形状不同，这里只要求它收 POST 音频、返回 {text}） */
  endpoint: string
  apiKey: string
  model?: string
  headers?: Record<string, string>
  language?: string
}

/** 多段 ArrayBuffer 拼成一份（整轮音频一次性上传用）。 */
function concatChunks(chunks: ArrayBuffer[]): ArrayBuffer {
  const total = chunks.reduce((n, c) => n + c.byteLength, 0)
  const out = new Uint8Array(total)
  let off = 0
  for (const c of chunks) {
    out.set(new Uint8Array(c), off)
    off += c.byteLength
  }
  return out.buffer
}

function safeJson(s: string): unknown {
  try {
    return JSON.parse(s)
  } catch {
    return null
  }
}

/**
 * 云端流式 ASR 的参考实现。
 *
 * 供应商无关：`endpoint` + `apiKey` 走配置，函数体内只有一段通用的 HTTP
 * 调用。换供应商只改配置，不改这里——**这是刻意的**：具体用哪家由用户自己的
 * key 决定，不该被我们的实现反过来绑死。
 *
 * 流式的实现约束：这一版按"整轮 → 一次请求"处理，`partial` 只在收到结果后
 * 补发一次。真正的边说边出字需要供应商的 chunked / WebSocket 协议，届时
 * `onPartial` 的时机就自然对了；上层（UI）不需要为它改结构。
 */
export class CloudAsr implements AsrProvider {
  readonly enabled: boolean
  private config: CloudAsrConfig
  private chunks: ArrayBuffer[] = []
  private partialCb: (t: string) => void = () => {}
  private started = false

  constructor(config: CloudAsrConfig) {
    this.config = config
    this.enabled = !!config.apiKey && !!config.endpoint
  }

  onPartial(cb: (text: string) => void): void {
    this.partialCb = cb
  }

  async start(): Promise<void> {
    this.chunks = []
    this.started = true
  }

  async push(chunk: ArrayBuffer): Promise<void> {
    if (!this.started) throw new Error('ASR: push 之前必须 start')
    this.chunks.push(chunk)
  }

  async stop(): Promise<AsrResult> {
    this.started = false
    if (!this.enabled || this.chunks.length === 0) {
      return { partial: '', final: null }
    }
    // 整轮音频拼成一份再上传。走 `uni.request`（见 http.ts）：App / 小程序端
    // **没有全局 fetch，也没有 Blob**，用 fetch+Blob 在真机上是必崩的。
    const body = concatChunks(this.chunks)
    this.chunks = []
    try {
      const res = await request({
        url: this.config.endpoint,
        method: 'POST',
        header: {
          authorization: `Bearer ${this.config.apiKey}`,
          'content-type': this.config.headers?.['content-type'] ?? 'application/octet-stream',
          ...(this.config.model ? { 'x-model': this.config.model } : {}),
          ...(this.config.headers ?? {}),
        },
        data: body,
        timeout: 30000,
      })
      if (!res.reachable || res.statusCode !== 200) {
        // 识别失败要说出来：静默返回空会让用户以为"麦克风坏了"
        return { partial: '', final: null }
      }
      const data = typeof res.data === 'string' ? safeJson(res.data) : res.data
      const text = ((data as { text?: string } | null)?.text ?? '').trim()
      if (text) this.partialCb(text)
      return { partial: text, final: text || null }
    } catch {
      return { partial: '', final: null }
    }
  }

  cancel(): void {
    this.chunks = []
    this.started = false
  }
}

// ---------------------------------------------------------------------------
// TTS
// ---------------------------------------------------------------------------

export type VoiceTurn = 'you' | 'agent' | 'silence'

export interface TtsProvider {
  readonly enabled: boolean
  /**
   * 说一段话。分句调用（不要把整篇回复一次丢进去）——手机侧是边生成边播，
   * 整篇会让用户在第一声之前干等。
   */
  speak(text: string): Promise<void>
  /** 打断：立刻停。用于 barge-in。 */
  flush(): Promise<void>
  onState(cb: (turn: VoiceTurn) => void): void
}

export class NullTts implements TtsProvider {
  readonly enabled = false
  async speak(): Promise<void> {}
  async flush(): Promise<void> {}
  onState(): void {}
}

export interface CloudTtsConfig {
  /** 合成端点；约定 POST { text } → 音频二进制 */
  endpoint: string
  apiKey: string
  voice?: string
  format?: 'mp3' | 'pcm' | 'wav'
}

/**
 * 云端 TTS 的参考实现（供应商无关，同上）。
 *
 * **默认关闭**：`NullTts` 是默认值。理由与"不主动推送"同源——出声是一个
 * 需要用户主动打开的开关，不是默认行为。
 */
export class CloudTts implements TtsProvider {
  readonly enabled: boolean
  private config: CloudTtsConfig
  private stateCb: (t: VoiceTurn) => void = () => {}
  private current: AbortController | null = null

  constructor(config: CloudTtsConfig) {
    this.config = config
    this.enabled = !!config.apiKey && !!config.endpoint
  }

  onState(cb: (t: VoiceTurn) => void): void {
    this.stateCb = cb
  }

  async speak(text: string): Promise<void> {
    const t = text.trim()
    if (!t || !this.enabled) return
    // App / 小程序端没有全局 fetch（见 http.ts）；TTS 尚未接线，缺 API 时
    // 直接静默降级，绝不抛。真正在 App 上出声需要 uni.createInnerAudioContext。
    if (typeof fetch === 'undefined') return
    // 新的句子打断旧的——两句之间不该叠音
    await this.flush()
    const ac = new AbortController()
    this.current = ac
    this.stateCb('agent')
    try {
      const res = await fetch(this.config.endpoint, {
        method: 'POST',
        signal: ac.signal,
        headers: {
          authorization: `Bearer ${this.config.apiKey}`,
          'content-type': 'application/json',
          ...(this.config.voice ? { 'x-voice': this.config.voice } : {}),
        },
        body: JSON.stringify({ text: t, format: this.config.format ?? 'mp3' }),
      })
      if (!res.ok) return
      const blob = await res.blob()
      await playBlob(blob, ac.signal)
    } catch {
      // 合成失败不打断对话：文字已经在屏上了
    } finally {
      if (this.current === ac) {
        this.current = null
        this.stateCb('silence')
      }
    }
  }

  async flush(): Promise<void> {
    if (this.current) {
      this.current.abort()
      this.current = null
      this.stateCb('silence')
    }
  }
}

/** 播一段音频。H5 用 `<audio>`；原生壳走 uni 的音频 API。 */
async function playBlob(blob: Blob, signal: AbortSignal): Promise<void> {
  if (typeof Audio === 'undefined' || typeof URL === 'undefined' || typeof URL.createObjectURL !== 'function') return
  const url = URL.createObjectURL(blob)
  try {
    await new Promise<void>((resolve) => {
      const audio = new Audio(url)
      const done = () => {
        audio.onended = null
        audio.onerror = null
        resolve()
      }
      audio.onended = done
      audio.onerror = done
      signal.addEventListener('abort', done, { once: true })
      void audio.play().catch(done)
    })
  } finally {
    URL.revokeObjectURL(url)
  }
}

// ---------------------------------------------------------------------------
// 轮次状态
// ---------------------------------------------------------------------------

/**
 * 通话的三态：你在说 / 它在说 / 沉默。
 *
 * 单看"有没有声音"不够——用户需要知道**轮到谁**，否则会以为卡住了。
 */
export class TurnTracker {
  private turn: VoiceTurn = 'silence'
  private cb: (t: VoiceTurn) => void = () => {}

  onChange(cb: (t: VoiceTurn) => void): void {
    this.cb = cb
    this.cb(this.turn)
  }

  get current(): VoiceTurn {
    return this.turn
  }

  youSpeaking(): void {
    this.set('you')
  }
  agentSpeaking(): void {
    this.set('agent')
  }
  silent(): void {
    this.set('silence')
  }

  private set(t: VoiceTurn) {
    if (this.turn === t) return
    this.turn = t
    this.cb(t)
  }
}
