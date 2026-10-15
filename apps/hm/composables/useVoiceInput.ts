import { ref } from 'vue'
import { CloudAsr, type AsrProvider } from '../core/voice'

/** 桌面端下发的语音识别配置帧体（voice_stt.rs）。 */
export interface VoiceSttWire {
  enabled?: boolean
  endpoint?: string
  api_key?: string
  model?: string | null
  language?: string | null
  audio_mime?: string
  extra_headers?: Record<string, string> | null
}

const VOICE_STT_KEY = 'hermix.voiceStt'

/**
 * 语音输入（长按说话 → 云端识别）。
 *
 * 只负责「录一轮 → 识别 → 给出文本」；识别出的文本通过 `onFinal` 交给调用方，
 * 是否落气泡 / 是否要高危二次确认由页面决定（那是界面的职责，不是录音的）。
 *
 * ASR 走桌面端下发的配置（WS 密封帧）；未配置时 `start()` 明确提示，不假装能听。
 */
export function useVoiceInput(onFinal: (text: string) => void) {
  const recording = ref(false)
  const partial = ref('')

  let asr: AsrProvider | null = null
  let recorder: UniHelper.RecorderManager | null = null

  /** 配置落地：帧下发与本地缓存共用这一条路径，避免两处构建逻辑漂移。 */
  function applyConfig(cfg: VoiceSttWire | null) {
    try {
      if (cfg) uni.setStorageSync(VOICE_STT_KEY, JSON.stringify(cfg))
      else uni.removeStorageSync(VOICE_STT_KEY)
    } catch {
      // 存储失败不阻塞：本次会话内 asr 仍然可用
    }
    if (cfg?.enabled && cfg.endpoint && cfg.api_key) {
      asr = new CloudAsr({
        endpoint: cfg.endpoint,
        apiKey: cfg.api_key,
        model: cfg.model ?? undefined,
        language: cfg.language ?? undefined,
        headers: {
          'content-type': cfg.audio_mime || 'audio/mpeg',
          ...(cfg.extra_headers ?? {}),
        },
      })
    } else {
      asr = null
    }
  }

  function loadSavedConfig() {
    try {
      const raw = uni.getStorageSync(VOICE_STT_KEY) as string | ''
      if (raw) applyConfig(JSON.parse(raw) as VoiceSttWire)
    } catch {
      // 缓存坏了等同未配置，等下一次帧下发覆盖
    }
  }

  /** App 端读录音临时文件为 ArrayBuffer。 */
  function readAudioFile(filePath: string): Promise<ArrayBuffer | null> {
    return new Promise((resolve) => {
      try {
        uni.getFileSystemManager().readFile({
          filePath,
          success: (res) => resolve(res.data as ArrayBuffer),
          fail: () => resolve(null),
        })
      } catch {
        resolve(null)
      }
    })
  }

  /** 平台录音管理器：整轮录 MP3，松手后读文件整体上传（比逐帧推兼容面宽）。 */
  function ensureRecorder(): UniHelper.RecorderManager {
    if (recorder) return recorder
    const rec = uni.getRecorderManager()
    rec.onStop((res) => {
      const provider = asr
      recording.value = false
      if (!provider) {
        partial.value = ''
        return
      }
      void (async () => {
        try {
          const buf = await readAudioFile(res.tempFilePath)
          if (buf?.byteLength) await provider.push(buf)
        } catch {
          // 读不到录音文件就按"没听清"走——不要静默假装成功
        }
        const result = await provider.stop()
        partial.value = ''
        const text = (result.final ?? '').trim()
        if (!text) {
          uni.showToast({ title: '没听清，请重试', icon: 'none' })
          return
        }
        onFinal(text)
      })()
    })
    recorder = rec
    return rec
  }

  async function start(): Promise<void> {
    if (!asr?.enabled) {
      // 桌面端没开语音识别就不假装能听：明确说，而不是按住没反应
      uni.showToast({ title: '桌面端未配置语音识别，仅支持文字发送', icon: 'none' })
      return
    }
    recording.value = true
    partial.value = ''
    const provider = asr
    provider.onPartial((t) => {
      partial.value = t
    })
    await provider.start()
    ensureRecorder().start({
      format: 'mp3',
      duration: 60000,
      sampleRate: 16000,
      numberOfChannels: 1,
    })
  }

  function stop(): void {
    if (!asr || !recording.value) return
    // 停录音触发 onStop，后续（读文件→上传→回显/高危确认）都在那里接续
    ensureRecorder().stop()
  }

  return { recording, partial, applyConfig, loadSavedConfig, start, stop }
}
