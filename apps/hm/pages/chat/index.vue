<script setup lang="ts">
/**
 * 对话页。
 *
 * 与审批、任务的分工：这里是「文档模型」——可回滚的消息流。通话是另一种
 * 心智模型，能从通话掉回这里。
 *
 * ## 改造重点（2026-10-09）
 *
 * 原版 `messages` 里塞着两条**假对话**（`把 tools 那一层的 dispatch 拆出来…`）。
 * 那不是占位示意，那是在界面上编了一段不存在的历史：用户分不清是真跑过还是
 * 演示，出问题也无从复现。现在初始就是空的，配了吉祥物空态。
 *
 * 工具轨迹从「一排彩色圆点」改成可折叠的步骤卡（参考 WorkBuddy 的
 * 「拆解为 N 个步骤 + 展开 + 完成态」）：手机上一步一行铺开，会把真正的
 * 回答顶出屏幕。
 */
import { ref, computed, nextTick, onMounted } from 'vue'
import { onUnmounted } from 'vue'
import { onReady, onShow } from '@dcloudio/uni-app'
import { isRiskyInstruction } from '../../core/voice'
import { dispatch, reduce, initialChatState, type ChatState } from '../../core/frames'
import { isPaired } from '../../core/pairing'
import { relayBaseUrl } from '../../core/clock'
import { ensureSplashClosed } from '../../core/boot'
import { ChatSession } from '../../core/session'
import { renderMarkdown } from '../../core/markdown'
import { newSessionId, loadSession, saveSession } from '../../core/history'
import { interruptSession } from '../../core/tasks'
import type { AsrProvider } from '../../core/voice'
import { CloudAsr } from '../../core/voice'

interface Bubble {
  id: string
  role: 'user' | 'assistant'
  text: string
  /** 语音发送的消息标记：这条是识别出来的，不是打字的 */
  viaVoice?: boolean
  /** 已完成气泡的工具轨迹（从 turnActive=false 时 capture） */
  doneTools?: string[]
  /** 助手气泡的实时工具轨迹（流式中从 chat.runningTools 同步） */
  runningTools?: string[]
}

const messages = ref<Bubble[]>([])
const draft = ref('')
const recording = ref(false)
const partial = ref('')
/** 高危指令：识别完不直接发，先问一次 */
const pendingRisky = ref<{ text: string } | null>(null)

/** 流式状态。reducer 是纯函数（见 core/frames），所以界面状态只有一条改法。 */
const chat = ref<ChatState>(initialChatState())

/** 流式中的 assistant 气泡。 */
const streaming = computed(() =>
  chat.value.streamingId ? { id: chat.value.streamingId, text: chat.value.streamingText } : null,
)

/** 空态：还没有任何一轮对话。 */
const isEmpty = computed(() => messages.value.length === 0 && !streaming.value)

/**
 * 未连接不是一道门，是主界面上的一个状态。
 *
 * 2026-10-09 改：原先冷启动直接把人按在配对屏，未配对等于整个 App 不可用。
 * 现在进得来、看得见任务与设置，只是发不了任务——「不能做什么」和
 * 「进不来」是两回事，后者对用户是纯粹的阻碍。
 */
const paired = ref(isPaired())
const hasRelay = ref(!!relayBaseUrl())

/** 键盘高度（来自 VisualViewport 检测），用于输入栏定位补偿。 */
const kbHeight = ref(0)

function setupKbHeight() {
  if (typeof window === 'undefined') return
  const vv = window.visualViewport
  if (!vv) return
  let lastKb = 0
  let rafId = 0
  const raf = window.requestAnimationFrame || ((fn: Function) => setTimeout(fn, 16))
  function compute() {
    const kb = Math.max(0, Math.round(window.innerHeight - vv.height - vv.offsetTop))
    return kb
  }
  function apply() {
    const kb = compute()
    if (Math.abs(kb - lastKb) < 6) return
    lastKb = kb
    kbHeight.value = kb
    document.documentElement.style.setProperty('--kb-height', kb + 'px')
  }
  function schedule() {
    if (rafId) return
    rafId = raf(() => { rafId = 0; apply() })
  }
  vv.addEventListener('resize', schedule)
  vv.addEventListener('scroll', schedule)
  window.addEventListener('resize', schedule)
  window.addEventListener('orientationchange', () => setTimeout(schedule, 200))
  // 初始计算
  setTimeout(schedule, 100)
}

// ── 会话持久化 ────────────────────────────────────────────────────
/** 当前会话 id（未发过消息时为 null，发过即创建） */
const currentSid = ref<string | null>(null)

onMounted(() => {
  setupKbHeight()
  // 恢复上次下发的语音识别配置（WS 帧没到前先能用旧值）
  loadSavedVoiceConfig()
  // 尝试恢复上一个被选中的会话
  const restore = uni.getStorageSync('hermix.restoreSession') as string | ''
  uni.removeStorageSync('hermix.restoreSession')
  if (restore) {
    const msgs = loadSession(restore)
    if (msgs.length) {
      messages.value = msgs
      currentSid.value = restore
      scrollToTail()
    }
  }
})

/** 每次轮次结束后保存会话（用户发消息时由 sendText 调用，助手回复落定后由 onFrame 调用） */
function persistSession() {
  if (!currentSid.value) return
  saveSession(currentSid.value, messages.value as any)
}


// ── 会话连接 ──────────────────────────────────────────────────────
//
// 链路状态是**主界面上的一个状态**，不是拦截层：连不上时对话区照常在，
// 只是发不出去并且说清楚为什么。早期版本把「没配对」做成整页拦截，
// 那是把「不能做什么」和「进不来」混成一件事。
const link = ref<'offline' | 'connecting' | 'online'>('offline')
const linkMsg = ref('')
let session: ChatSession | null = null

function ensureSession() {
  if (session || !paired.value) return
  session = new ChatSession({
    onPlain: (json) => onFrame(json),
    onState: (s) => {
      if (s === 'open') {
        link.value = 'online'
        linkMsg.value = ''
      } else if (s === 'connecting') {
        link.value = 'connecting'
      } else if (s === 'closed' || s === 'error') {
        link.value = 'connecting' // RelayClient 自己会退避重连，别谎报成断开
        linkMsg.value = ''
      }
    },
    onError: (m) => {
      linkMsg.value = m
    },
  })
  void session.start()
}

// 页面可能被反复进入（reLaunch 回来、tab 切换），onShow 里 start 是幂等的，
// 不会叠连接。
onShow(() => {
  paired.value = isPaired()
  if (paired.value) ensureSession()
})

// boot 页在 onLoad 里 reLaunch，其 onReady 大概率被跳过；这里是正常路径的
// 兜底关点（首屏渲染完成后关启动图，根因见 core/boot.ts 的 ensureSplashClosed）。
onReady(() => {
  ensureSplashClosed()
})

const anchor = ref('')
function scrollToTail() {
  void nextTick(() => {
    anchor.value = ''
    void nextTick(() => (anchor.value = 'tail'))
  })
}

/** 收一帧 —— **唯一**的入站入口。UI 状态只能从这里变。 */
function onFrame(raw: string) {
  // 桌面端下发的语音识别配置（WS 密封帧，relay 不可见）：不进 reducer，
  // 就地生效并持久化——重连后拿到新配置前沿用旧值，避免长按失灵。
  try {
    const f = JSON.parse(raw) as { type?: string; voice_stt?: VoiceSttWire }
    if (f?.type === 'voice.stt_config') {
      applyVoiceConfig(f.voice_stt)
      return
    }
  } catch {
    // 非 JSON 不可能走到这，但保底按原路径处理
  }
  const prev = chat.value.streamingId
  const next = reduce(chat.value, dispatch(raw))
  // turn.done 是唯一的"一轮落定"信号；doneTools 在此时已达最终态
  const doneAtTurnEnd = !next.turnActive && prev && next.streamingId
  chat.value = doneAtTurnEnd
    ? { ...next, streamingId: null, streamingText: '' }
    : next
  if (doneAtTurnEnd) {
    messages.value.push({
      id: prev!,
      role: 'assistant',
      text: next.streamingText,
      doneTools: [...next.doneTools],
    })
    persistSession()
  }
  scrollToTail()
}

let asr: AsrProvider | null = null

/** 桌面端下发的语音识别配置帧体（voice_stt.rs）。 */
interface VoiceSttWire {
  enabled?: boolean
  endpoint?: string
  api_key?: string
  model?: string | null
  language?: string | null
  audio_mime?: string
  extra_headers?: Record<string, string> | null
}

const VOICE_STT_KEY = 'hermix.voiceStt'

/** 配置落地：帧下发与本地缓存共用这一条路径，避免两处构建逻辑漂移。 */
function applyVoiceConfig(cfg: VoiceSttWire | null) {
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

function loadSavedVoiceConfig() {
  try {
    const raw = uni.getStorageSync(VOICE_STT_KEY) as string | ''
    if (raw) applyVoiceConfig(JSON.parse(raw) as VoiceSttWire)
  } catch {
    // 缓存坏了等同未配置，等下一次帧下发覆盖
  }
}

/** 平台录音管理器：整轮录 MP3，松手后读文件整体上传（比逐帧推兼容面宽）。 */
let recorder: UniHelper.RecorderManager | null = null
function ensureRecorder(): UniHelper.RecorderManager {
  if (recorder) return recorder
  const rec = uni.getRecorderManager()
  rec.onStop((res) => {
    const provider = asr
    recording.value = false
    void (async () => {
      try {
        const buf = await readAudioFile(res.tempFilePath)
        if (buf?.byteLength) await provider.push(buf)
      } catch {
        // 读不到录音文件就按"没听清"走——不要静默假装成功
      }
      const result = await provider.stop()
      const text = (result.final ?? '').trim()
      partial.value = ''
      if (!text) {
        uni.showToast({ title: '没听清，请重试', icon: 'none' })
        return
      }
      // 回显成用户气泡（还没发出去）
      messages.value.push({ id: `v${Date.now()}`, role: 'user', text, viaVoice: true })
      scrollToTail()
      if (isRiskyInstruction(text)) {
        // 对**指令本身**的确认，与工具审批是两件事
        pendingRisky.value = { text }
      }
    })()
  })
  recorder = rec
  return rec
}

/** App 端读录音临时文件为 ArrayBuffer。 */
function readAudioFile(filePath: string): Promise<ArrayBuffer | null> {
  return new Promise((resolve) => {
    try {
      const fsm = uni.getFileSystemManager()
      fsm.readFile({
        filePath,
        success: (res) => resolve(res.data as ArrayBuffer),
        fail: () => resolve(null),
      })
    } catch {
      resolve(null)
    }
  })
}

async function onLongPressStart() {
  if (!asr?.enabled) {
    // 桌面端没开语音识别就不假装能听：明确说，而不是按住没反应
    uni.showToast({ title: '桌面端未配置语音识别，仅支持文字发送', icon: 'none' })
    return
  }
  recording.value = true
  partial.value = ''
  asr.onPartial((t) => {
    partial.value = t
  })
  await asr.start()
  ensureRecorder().start({
    format: 'mp3',
    duration: 60000,
    sampleRate: 16000,
    numberOfChannels: 1,
  })
}

function onLongPressEnd() {
  if (!asr || !recording.value) return
  // 停录音触发 onStop，后续（读文件→上传→回显/高危确认）都在那里接续
  ensureRecorder().stop()
}

function confirmRisky() {
  if (pendingRisky.value) sendText(pendingRisky.value.text)
  pendingRisky.value = null
}

function goConnect() {
  uni.navigateTo({ url: '/pages/pairing/index' })
}

function goSettings() {
  uni.navigateTo({ url: '/pages/settings/index' })
}

function goHistory() {
  uni.navigateTo({ url: '/pages/history/index' })
}

function confirmNew() {
  if (!messages.value.length && !streaming.value) return
  uni.showModal({
    title: '新对话',
    content: '清空当前上下文，开始一个新的会话？',
    confirmText: '新对话',
    success: (res) => {
      if (!res.confirm) return
      // 保存当前会话（如果有过消息）
      if (currentSid.value) {
        saveSession(currentSid.value, messages.value as any)
      }
      messages.value = []
      chat.value = initialChatState()
      currentSid.value = null
    },
  })
}

function send() {
  sendText(draft.value)
  draft.value = ''
}

function sendText(t: string) {
  const text = t.trim()
  if (!text) return
  // 首次发消息 → 创建会话 id 并落盘
  if (!currentSid.value) {
    currentSid.value = newSessionId()
    saveSession(currentSid.value, [])
  }
  ensureSession()
  void (async () => {
    // 先发出去，成功了才落气泡：连不上时留一条永远等不到回复的假消息，
    // 比直接不落更糟——用户会以为电脑收到了。
    const ok = (await session?.send(text)) ?? false
    if (!ok) {
      uni.showToast({ title: linkMsg.value || '还没连上电脑，正在重连', icon: 'none' })
      return
    }
    // 打字发的补一条（语音已经回显过气泡了）
    if (!messages.value.some((m) => m.role === 'user' && m.text === text && m.viaVoice)) {
      messages.value.push({ id: String(Date.now()), role: 'user', text })
    }
    persistSession()
    // turnActive / 工具轨迹 / 三态回执都从回来的帧里推（见 onFrame），
    // **不在这里自己置 true** —— 那样"发出去"和"服务端开始跑"就分不开了。
    scrollToTail()
  })()
}

/**
 * 中断。只有 `turnActive` 为真时这个按钮才出现——那表示**服务端确实在跑**，
 * 所以它不是常驻的摆设按钮。
 */
async function stop() {
  const res = await interruptSession(currentSid.value)
  if (res.interrupted) {
    uni.showToast({ title: '已中断', icon: 'none' })
  } else {
    uni.showToast({ title: res.reason ?? '中断失败', icon: 'none' })
  }
}

onUnmounted(() => {
  draft.value = ''
  // 连接跟着页面走：留着会让一条没人读的 socket 常驻，退避重连也停不下来。
  session?.stop()
  session = null
})
</script>

<template>
  <hm-shell title="对话" :scroll-into-view="anchor" :class="{ 'kb-open': kbHeight > 120 }">
    <template #end>
      <hm-model-picker />
      <view class="hist-btn" @tap="goHistory"><text>☰</text></view>
      <view class="new-btn" @tap="confirmNew"><text>✛</text></view>
    </template>
    <!-- 未连接：主界面里的一张卡片，不是拦截层 -->
    <view v-if="!paired" class="connect">
      <view class="c-top">
        <view class="c-dot" />
        <text class="c-title">还没有连接电脑</text>
      </view>
      <text class="c-hint">
        {{
          hasRelay
            ? '连接之后，你在这里下的任务会在电脑上执行，审批和结果也回到这里。'
            : '扫电脑上的配对码即可连接：relay 地址与凭据会自动带上，无需手填。'
        }}
      </text>
      <view class="c-acts">
        <view class="c-btn pri" @tap="goConnect"><text>扫码连接电脑</text></view>
        <view class="c-btn" @tap="goSettings"><text>设置</text></view>
      </view>
    </view>

    <!-- 链路状态：连不上时对话区照常在，只是明确说清为什么发不出去 -->
    <view v-if="paired && link !== 'online'" class="link-bar">
      <view class="link-dot" />
      <text class="link-txt">{{ linkMsg || '正在连电脑…' }}</text>
    </view>

    <hm-empty
      v-else-if="isEmpty"
      title="还没有对话"
      hint="在下面输入一句话下任务。任务实际在你的电脑上跑，这里只做入口与呈现。"
    >
      <image class="mascot" src="/static/mascot.png" mode="aspectFit" />
    </hm-empty>

    <view v-for="m in messages" :key="m.id" :class="['bub', m.role]">
      <hm-step v-if="m.role === 'assistant'" :running="m.runningTools ?? []" :done="m.doneTools ?? []" />
      <rich-text v-if="m.role === 'assistant'" class="txt md" :nodes="renderMarkdown(m.text)" />
      <text v-else class="txt">{{ m.text }}</text>
      <text v-if="m.viaVoice" class="via">语音 · 长按可改</text>
    </view>

    <view v-if="recording" class="bub user live">
      <text class="txt">{{ partial || '听着呢…' }}</text>
    </view>

    <!-- 流式中的助手气泡：单独一个壳，收尾时转成正式气泡 -->
    <view v-if="streaming" class="bub assistant">
      <hm-step :running="chat.runningTools" :done="chat.doneTools" />
      <rich-text class="txt md" :nodes="renderMarkdown(streaming.text)" />
    </view>

    <view v-if="chat.busy" class="notice warn"><text>上一轮还在跑 · 你发的话会排队</text></view>
    <view v-else-if="chat.turnActive" class="notice"><text>还在想 · 下一条心跳 60s 后</text></view>
    <view v-if="chat.lastError" class="notice err"><text>{{ chat.lastError }}</text></view>

    <view id="tail" class="tail" />

    <!-- 高危指令发前确认：对指令本身，不是工具审批 -->
    <view v-if="pendingRisky" class="risky">
      <text class="risky-t">确认发送这条？</text>
      <text class="risky-b">{{ pendingRisky.text }}</text>
      <view class="risky-keys">
        <view class="key pri" @tap="confirmRisky"><text>发送</text></view>
        <view class="key" @tap="pendingRisky = null"><text>取消</text></view>
      </view>
    </view>

    <template #bottom>
      <view class="composer">
        <view v-if="!paired" class="hint off">未连接电脑，暂时发不了任务</view>
        <view v-else-if="chat.turnActive" class="hint">运行中 · 你发的话会排队</view>
        <view class="field">
          <input
            v-model="draft"
            class="inp"
            :placeholder="paired ? '说点什么…' : '先连接电脑'"
            :disabled="!paired"
            confirm-type="send"
            :adjust-position="true"
            @confirm="send"
          />
          <view
            class="mic"
            :class="{ on: recording }"
            @longpress="onLongPressStart"
            @touchend="onLongPressEnd"
          >
            <text>{{ recording ? '松开发送' : '按住说' }}</text>
          </view>
          <!-- 运行中就地变停止键：和 WorkBuddy 一致，少一次找按钮的时间 -->
          <view v-if="chat.turnActive" class="send stop" @tap="stop"><text>■</text></view>
          <view v-else class="send" :class="{ off: !paired }" @tap="paired ? send() : undefined">
            <text>↑</text>
          </view>
        </view>
      </view>
    </template>
  </hm-shell>
</template>

<style lang="scss" scoped>
.mascot {
  width: 180rpx;
  height: 180rpx;
  margin-top: 40rpx;
  opacity: 0.9;
}

/* 未连接卡片：与空态同一个位置，只是多一个明确的下一步。 */
.connect {
  margin: 40rpx 26rpx;
  padding: 28rpx;
  background: $card;
  border-radius: $r-card;
}

/* 链路条：不是拦截层，是一行状态。靠底色差与留白分层，不描边。 */
.link-bar {
  display: flex;
  align-items: center;
  gap: 12rpx;
  margin: 20rpx 26rpx 0;
  padding: 16rpx 20rpx;
  background: $card;
  border-radius: $r-card;
}
.link-dot {
  width: 14rpx;
  height: 14rpx;
  border-radius: 50%;
  background: $warn;
}
.link-txt {
  font-size: $fs-sub;
  color: $fg2;
}
.c-top {
  display: flex;
  align-items: center;
  gap: 14rpx;
}
.c-dot {
  width: 16rpx;
  height: 16rpx;
  border-radius: 50%;
  background: $warn;
}
.c-title {
  font-size: $fs-body;
  font-weight: 600;
  color: $fg;
}
.c-hint {
  display: block;
  margin-top: 12rpx;
  font-size: $fs-sub;
  color: $fg2;
  line-height: 1.6;
}
.c-acts {
  display: flex;
  gap: 14rpx;
  margin-top: 24rpx;
}
.c-btn {
  flex: 1;
  height: $tap;
  line-height: $tap;
  text-align: center;
  border-radius: 44rpx;
  background: $raised;
  color: $fg;
  font-size: $fs-sub;
}
.c-btn.pri {
  background: $accent;
  color: $on-accent;
}

/* 气泡靠底色分层：用户=$raised（比页底高两档），助手=$card（高一档）。 */
.bub {
  max-width: 84%;
  padding: 20rpx 24rpx;
  border-radius: $r-bubble;
  margin: 0 $gutter 20rpx;
  font-size: $fs-body;
  line-height: 1.55;
  box-sizing: border-box;
}
.bub.user {
  margin-left: auto;
  background: $raised;
  border-bottom-right-radius: 8rpx;
}
.bub.assistant {
  background: $card;
  border-bottom-left-radius: 8rpx;
}
.bub.live {
  border-bottom-right-radius: 8rpx;
}
.txt {
  display: block;
  color: $fg;
  word-break: break-word;
}
.via {
  display: block;
  margin-top: 8rpx;
  font-size: $fs-mini;
  color: $fg4;
}
.tail {
  height: 8rpx;
}

.notice {
  margin: 0 $gutter 20rpx;
  padding: 16rpx 20rpx;
  border-radius: 18rpx;
  font-size: $fs-mini;
  color: $fg2;
  background: $card;
}
.notice.warn {
  background: $warn-bg;
  color: $warn;
}
.notice.err {
  background: $err-bg;
  color: $err;
}

.risky {
  margin: 0 $gutter 20rpx;
  background: $warn-bg;
  border-radius: 18rpx;
  padding: 20rpx 22rpx;
}
.risky-t {
  display: block;
  font-size: $fs-sub;
  font-weight: 600;
  color: $warn;
}
.risky-b {
  display: block;
  margin-top: 6rpx;
  margin-bottom: 16rpx;
  font-size: $fs-mini;
  color: $fg2;
}
.risky-keys {
  display: flex;
  gap: 14rpx;
}
.key {
  flex: 1;
  height: $tap;
  line-height: $tap;
  text-align: center;
  border-radius: 44rpx;
  font-size: $fs-sub;
  background: $raised;
  color: $fg2;
}
.key.pri {
  background: $accent;
  color: $on-accent;
}

/* 输入栏：与滚动区之间靠底色差分界，不画线。 */
.composer {
  flex: none;
  background: $base;
  padding: 16rpx 22rpx calc(env(safe-area-inset-bottom) + 20rpx);
}
.hint {
  display: block;
  font-size: $fs-mini;
  color: $fg2;
  margin-bottom: 10rpx;
}
.hint.off {
  color: $fg4;
}
.field {
  display: flex;
  align-items: center;
  height: 96rpx;
  background: $card;
  border-radius: 48rpx;
  padding: 0 12rpx 0 28rpx;
}
.inp {
  flex: 1;
  min-width: 0;
  color: $fg;
  font-size: $fs-body;
}
.mic {
  flex: none;
  padding: 0 20rpx;
  height: 88rpx;
  line-height: 88rpx;
  border-radius: 44rpx;
  background: $raised;
  color: $fg2;
  font-size: $fs-mini;
}
.mic.on {
  background: $accent;
  color: $on-accent;
}
.send {
  flex: none;
  width: 88rpx;
  height: 88rpx;
  line-height: 88rpx;
  text-align: center;
  border-radius: 44rpx;
  background: $accent;
  color: $on-accent;
  margin-left: 12rpx;
  font-size: 30rpx;
}
.send.stop {
  background: $err-bg;
  color: $err;
}
.send.off {
  opacity: 0.35;
}

/* 顶栏按钮：历史列表 + 新建对话 */
:deep(.hist-btn),
:deep(.new-btn) {
  flex: none;
  width: 64rpx;
  height: 64rpx;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 32rpx;
  background: $raised;
  color: $fg2;
  font-size: 34rpx;
}
:deep(.hist-btn:active),
:deep(.new-btn:active) {
  opacity: 0.7;
}
</style>
