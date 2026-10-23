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
import { ref, computed, nextTick, onMounted, onUnmounted } from 'vue'
import { onReady, onShow } from '@dcloudio/uni-app'
import { isRiskyInstruction } from '../../core/voice'
import { dispatch, reduce, initialChatState, type ChatState } from '../../core/frames'
import { noteApprovalRequest, forgetApproval } from '../../core/approvalQueue'
import { isPaired } from '../../core/pairing'
import { relayBaseUrl } from '../../core/clock'
import { ensureSplashClosed } from '../../core/boot'
import { ChatSession } from '../../core/session'
import { renderMarkdown } from '../../core/markdown'
import {
  newSessionId,
  loadSession,
  saveSession,
  listSessions,
  type ChatSource,
} from '../../core/history'
// 阿黎是手机上唯一的对话对象，不再有"来源切换"。
import { interruptSession } from '../../core/tasks'
import { loadModelConfig, saveModelConfig, clearModelConfig } from '../../core/modelStore'
import { streamLocalChat, type LocalTurn } from '../../core/localChat'
import type { ModelConfig } from '../../core/model'
import { useKeyboardHeight } from '../../composables/useKeyboardHeight'
import { useVoiceInput, type VoiceSttWire } from '../../composables/useVoiceInput'

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
  /** 消息已发到网络层，等待桌面端开始处理（收到 turn.busy / assistant.delta 首帧时清除） */
  sending?: boolean
  /** 这一轮跑了多久（秒），落定后冻结，用于气泡页脚的「时长」 */
  duration?: number
  /** 这一轮的思考过程（<think> 折叠块），落定时从会话状态带走 */
  thinking?: string
}

const messages = ref<Bubble[]>([])
const draft = ref('')
/** 高危指令：识别完不直接发，先问一次 */
const pendingRisky = ref<{ text: string } | null>(null)

/** 流式状态。reducer 是纯函数（见 core/frames），所以界面状态只有一条改法。 */
const chat = ref<ChatState>(initialChatState())

/** 流式中的 assistant 气泡。 */
const streaming = computed(() =>
  chat.value.streamingId ? { id: chat.value.streamingId, text: chat.value.streamingText } : null,
)

// ── 打字机 + 时长（抄 dsh-mobile 的「字符级逐字 + 实时执行可视化」）──
//
// 流式文本**逐字**显示：typedText 落后于 chat.streamingText，由定时器追平，
// 最新一个字符带淡入 + 光标，读起来就是对方在「现场打字」。落定后气泡切回
// markdown 渲染（见 onFrame）。时长与打字机共用一轮生命周期。
const typedText = ref('')
const turnStart = ref(0)
const elapsed = ref(0)
const lastDuration = ref<number | null>(null)
/**
 * 电脑端**没有**开启「模型配置推送」→ 手机端不能本地对话，且本地存的那份
 * （含 API Key）已被删除。界面上要说清是「电脑端没开」而不是「手机坏了」。
 */
const modelPushOff = ref(false)
let typer: ReturnType<typeof setInterval> | null = null
let ticker: ReturnType<typeof setInterval> | null = null

function startTyper() {
  stopTyper()
  // 每 tick 追几字符，长文本不至于跟不上流入；短文本也不会一顿一顿。
  typer = setInterval(() => {
    const full = chat.value.streamingText
    const have = typedText.value.length
    if (have < full.length) {
      const step = Math.max(2, Math.ceil((full.length - have) / 12))
      typedText.value = full.slice(0, have + step)
    }
  }, 24)
}
function stopTyper() {
  if (typer !== null) {
    clearInterval(typer)
    typer = null
  }
}
function startTicker() {
  stopTicker()
  ticker = setInterval(() => {
    if (turnStart.value) elapsed.value = (Date.now() - turnStart.value) / 1000
  }, 200)
}
function stopTicker() {
  if (ticker !== null) {
    clearInterval(ticker)
    ticker = null
  }
}
/** 秒 → "12.3s" 或 "1:05"（长任务用 m:ss，和对方 ContextMeter 同语气）。 */
function fmtDur(s: number): string {
  if (s >= 60) return `${Math.floor(s / 60)}:${String(Math.floor(s % 60)).padStart(2, '0')}`
  return `${s.toFixed(1)}s`
}
/** 气泡页脚「复制」——助手气泡的操作条第一项（窄屏单行横滚）。 */
function copy(text: string) {
  uni.setClipboardData({
    data: text,
    success: () => uni.showToast({ title: '已复制', icon: 'none' }),
  })
}
/** 上下文仪表文案：used/max 用 k 为单位，配百分比。 */
function fmtCtx(c: { used: number; max: number; percent: number }): string {
  return `${Math.round(c.used / 1000)}k / ${Math.round(c.max / 1000)}k (${Math.round(c.percent)}%)`
}
/** To-dos 卡片的状态图标。 */
function todoIcon(status: string): string {
  if (status === 'completed' || status === 'done') return '✓'
  if (status === 'in_progress') return '●'
  return '○'
}
/**
 * 手机端切换默认模型：走同一条密封上行（WS），不走 REST。
 *
 * 中继是只路由的字节管道，没有通往桌面的 HTTP 通道——手机此前那条
 * `GET {relay}/api/model/list` 必然 404，模型清单因此永远是空的。
 * 桌面收到后会回推一份新清单（`model.list`），这里不必自己改本地状态。
 */
async function onSelectModel(model: string): Promise<void> {
  const ok = await session?.selectModel(model)
  if (!ok) {
    uni.showToast({ title: '没连上电脑，切换失败', icon: 'none' })
  }
}

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

/** 键盘高度补偿（VisualViewport 检测），用于输入栏定位补偿。 */
const { kbHeight } = useKeyboardHeight()

// ── 会话持久化 ────────────────────────────────────────────────────
/** 当前会话 id（未发过消息时为 null，发过即创建） */
const currentSid = ref<string | null>(null)

onMounted(() => {
  // 恢复上次下发的语音识别配置（WS 帧没到前先能用旧值）
  loadSavedVoiceConfig()
  // 来源（阿黎/桌面）由阿黎页或历史页写进存储后再切过来——tab 页不收 query。
  source.value = getChatSource()
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
  } else {
    // 没有指定会话：接着最近一条往下聊，而不是每次进来都是一片空白。
    restoreLatest()
  }
})

/** 每次轮次结束后保存会话（用户发消息时由 sendText 调用，助手回复落定后由 onFrame 调用） */
function persistSession() {
  if (!currentSid.value) return
  saveSession(currentSid.value, messages.value, source.value)
}


// ── 会话连接 ──────────────────────────────────────────────────────
//
// 链路状态是**主界面上的一个状态**，不是拦截层：连不上时对话区照常在，
// 只是发不出去并且说清楚为什么。早期版本把「没配对」做成整页拦截，
// 那是把「不能做什么」和「进不来」混成一件事。
const link = ref<'offline' | 'connecting' | 'online'>('offline')
const linkMsg = ref('')
/** 凭据被中转拒绝（终态，不会自动恢复）：横幅要给出处理入口。 */
const linkRevoked = computed(() => /已被中转拒绝/.test(linkMsg.value))
let session: ChatSession | null = null

/** 返回等连接就绪的 Promise；已就绪时 resolve 得很快。 */
async function ensureSession(): Promise<void> {
  if (session?.connected) return
  if (session) return session.start()
  if (!paired.value) return
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
      // 凭据被撤销（终态，不会自愈）→ 这台手机再也连不上这台电脑了。本地
      // 存的模型配置（含 API Key）必须一并删掉：撤销的意义是「这台设备不再
      // 持有访问权」，留着 key 等于只撤了一半。
      if (/401|凭据已被撤销|已被中转拒绝/i.test(m)) {
        clearModelConfig()
        modelPushOff.value = true
      }
    },
  })
  await session.start()
}

// 页面可能被反复进入（reLaunch 回来、tab 切换），onShow 里 start 是幂等的，
// 不会叠连接。
onShow(() => {
  paired.value = isPaired()
  // 来自阿黎页 / 历史页的跳转：来源与目标会话都靠存储传（tab 页不收 query）。
  // 来源变了就把界面清干净——两个来源的历史分开存，把 A 的气泡留在 B 的
  // 界面上，用户会以为"刚才和阿黎说的话被电脑看到了"。
  const pending = uni.getStorageSync('hermix.restoreSession') as string | ''
  if (pending) {
    uni.removeStorageSync('hermix.restoreSession')
    const msgs = loadSession(pending)
    if (msgs.length) {
      messages.value = msgs
      currentSid.value = pending
      scrollToTail()
    }
  }
  // 先装上次从电脑收到的配置：电脑没连上（或还没把配置推过来）时，模型
  // 选择器不该是空的——本地直连也靠这份凭据。
  if (!chat.value.models.length) {
    const saved = loadModelConfig()
    if (saved) {
      chat.value = {
        ...chat.value,
        models: saved.models,
        providers: saved.providers,
        defaultModel: saved.defaultModel,
      }
    }
  }
  // session 已死（401 撤销，closed=true）时清掉它，让 ensureSession 走 _doStart 重建。
  // 正在连接中（isConnecting=true）的不动——让它继续，不要打断。
  if (session && !session.connected && !session.isConnecting) {
    session = null
  }
  if (paired.value) void ensureSession()
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
  const prevTurnActive = chat.value.turnActive
  const ev = dispatch(raw)
  // 审批是消息，不是「另一条 REST 上的资源」。帧到手就记一笔，审批页会立刻
  // 去拉一次，而不是等下一个轮询周期——重连补发回来的审批也走这条路径。
  if (ev.kind === 'approval.request') {
    noteApprovalRequest({
      requestId: ev.requestId,
      tool: ev.tool,
      sid: currentSid.value ?? undefined,
      receivedAt: Date.now(),
    })
  }
  // 落定前要先把这一轮的思考过程从会话状态取走（reduce 的 turn.done 会清空它）
  const thinking = chat.value.thinking
  const next = reduce(chat.value, ev)
  // 电脑推来的模型配置 → 存一份本地：电脑关机后这份就是唯一可用的配置
  // （本地直连要用它），电脑改了配置手机下次连上就跟着变。
  //
  // **空清单 = 电脑端没开这个开关**（或它没有可用模型）。这时手机端必须
  // **删掉本地存的那份**（含 API Key）：只留着不删，等于「电脑说不给了，
  // 手机还拿着旧 key 继续用」——用户以为收干净了，其实没有。
  if (ev.kind === 'model.list') {
    if (!next.models.length) {
      clearModelConfig()
      modelPushOff.value = true
    } else {
      saveModelConfig({
        models: next.models,
        providers: next.providers,
        defaultModel: next.defaultModel,
      })
      modelPushOff.value = false
    }
  }
  // 桌面开始处理的第一帧：清除所有用户消息的 sending 状态
  if (!prevTurnActive && next.turnActive) {
    messages.value.forEach((m) => { m.sending = false })
    // 新一轮：重置打字机与计时（时长从这一刻起算）
    typedText.value = ''
    turnStart.value = Date.now()
    elapsed.value = 0
    startTyper()
    startTicker()
  }
  // turn.done 是唯一的"一轮落定"信号；doneTools 在此时已达最终态
  const doneAtTurnEnd = !next.turnActive && prev && next.streamingId
  if (doneAtTurnEnd) {
    stopTyper()
    stopTicker()
    lastDuration.value = turnStart.value ? (Date.now() - turnStart.value) / 1000 : null
  }
  chat.value = doneAtTurnEnd
    ? { ...next, streamingId: null, streamingText: '' }
    : next
  if (doneAtTurnEnd) {
    messages.value.push({
      id: prev!,
      role: 'assistant',
      text: next.streamingText,
      doneTools: [...next.doneTools],
      duration: lastDuration.value ?? undefined,
      thinking: thinking || undefined,
    })
    typedText.value = ''
    persistSession()
  }
  scrollToTail()
}

// ── 工具审批 ──────────────────────────────────────────────────────
//
// 审批是对话里的一条消息：`approval.request` 停在 `chat.pendingApproval` 上等人点，
// 点完把决定**从同一条上行**发回 PC（`approval_response`），agent 才继续往下跑。
// 不做本地先行结算——结果以 PC 的真实进展为准。
const approvalBusy = ref(false)

async function answerApproval(decision: 'approved' | 'denied') {
  const pending = chat.value.pendingApproval
  if (!pending || approvalBusy.value) return
  if (!session) return
  approvalBusy.value = true
  try {
    const sent = await session.respondApproval(pending.requestId, decision)
    if (!sent) {
      // 通道没就绪（正在重连 / 未配对）：别假装成功，用户会以为批过了
      uni.showToast({ title: '没发出去，检查连接后重试', icon: 'none' })
      return
    }
    chat.value = reduce(chat.value, {
      kind: 'approval.answered',
      requestId: pending.requestId,
    })
    forgetApproval(pending.requestId)
  } finally {
    approvalBusy.value = false
  }
}

// ── 语音输入 ──────────────────────────────────────────────────────
// 长按说话 → 云端识别 → 回显气泡。录音/上传细节在 composables/useVoiceInput；
// 「落气泡」与「高危二次确认」是界面职责，留在页面。
const {
  recording,
  partial,
  applyConfig: applyVoiceConfig,
  loadSavedConfig: loadSavedVoiceConfig,
  start: onLongPressStart,
  stop: onLongPressEnd,
} = useVoiceInput((text) => {
  // 回显成用户气泡（还没发出去）
  messages.value.push({ id: `v${Date.now()}`, role: 'user', text, viaVoice: true })
  scrollToTail()
  if (isRiskyInstruction(text)) {
    // 对**指令本身**的确认，与工具审批是两件事
    pendingRisky.value = { text }
  }
})

function confirmRisky() {
  if (pendingRisky.value) sendText(pendingRisky.value.text)
  pendingRisky.value = null
}

function goConnect() {
  // 清理旧 session：旧设备的 token 在撤销后已无效，不清会让下一条消息
  // 继续用旧 RelayClient（closed=true）白等。stop 后 ensureSession 会重建。
  session?.stop()
  session = null
  uni.navigateTo({ url: '/pages/pairing/index' })
}

function goSettings() {
  uni.navigateTo({ url: '/pages/settings/index' })
}

function goHistory() {
  // 阿黎的对话在自己的专属页（她的历史只属于她）；桌面来源走通用历史列表。
  uni.navigateTo({
    url: source.value === 'ali' ? '/pages/ali/index' : '/pages/history/index',
  })
}

function goCall() {
  uni.navigateTo({ url: '/pages/call/index' })
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
        saveSession(currentSid.value, messages.value)
      }
      messages.value = []
      chat.value = initialChatState()
      currentSid.value = null
    },
  })
}

/**
 * 手机上只有一个对话对象：**阿黎**。
 *
 * 她在**电脑上**执行——工具、工作区、记忆，以及随时派 agent 角色干活，全都
 * 发生在桌面端；手机只是她的遥控入口，**不在手机上跑模型**。所以这里没有
 * 「来源切换」：换来源就等于换人，而用户要的就是她。
 */
const source = ref<ChatSource>('ali')

/** 恢复最近一条会话（没有就留空）。 */
function restoreLatest() {
  const latest = listSessions('ali')[0]
  if (!latest) return
  const msgs = loadSession(latest.id)
  if (!msgs.length) return
  messages.value = msgs
  currentSid.value = latest.id
}

function send() {
  sendText(draft.value)
  draft.value = ''
}

function sendText(t: string) {
  const text = t.trim()
  if (!text) return
  // 首次发消息 → 创建会话 id 并落盘（id 带来源前缀，排障时一眼看出归属）
  if (!currentSid.value) {
    currentSid.value = newSessionId(source.value)
    saveSession(currentSid.value, [], source.value)
  }
  void (async () => {
    // 等连接真正就绪（hello + key.exchange 完成）再发，否则 send() 返回 false
    // 导致消息「发出去没回」而用户以为是网络慢。
    await ensureSession()
    // 先发出去，成功了才落气泡：连不上时留一条永远等不到回复的假消息，
    // 比直接不落更糟——用户会以为电脑收到了。
    const ok = (await session?.send(text)) ?? false
    if (!ok) {
      // 发不出去就明说。**不静默改投任何别的通路**：用户以为话发给了
      // 阿黎（她跑在电脑上），实际发去别处，比发不出去严重得多。
      uni.showToast({
        title: linkMsg.value || '没连上电脑，正在重连',
        icon: 'none',
      })
      return
    }
    // 打字发的补一条（语音已经回显过气泡了）
    if (!messages.value.some((m) => m.role === 'user' && m.text === text && m.viaVoice)) {
      messages.value.push({ id: String(Date.now()), role: 'user', text, sending: true })
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
  stopTyper()
  stopTicker()
  // 连接跟着页面走：留着会让一条没人读的 socket 常驻，退避重连也停不下来。
  session?.stop()
  session = null
})
</script>

<template>
  <hm-shell title="阿黎" :scroll-into-view="anchor" :class="{ 'kb-open': kbHeight > 120 }">
    <template #end>
      <hm-model-picker
        :models="chat.models"
        :default-model="chat.defaultModel"
        @select="onSelectModel"
      />
      <view class="hist-btn" @tap="goHistory"><text>☰</text></view>
      <!-- 通话：当前对话的另一种交互模式，入口挂在这条对话上，不占底部频道位 -->
      <view class="call-btn" @tap="goCall"><text>☎</text></view>
      <view class="new-btn" @tap="confirmNew"><text>✛</text></view>
    </template>
    <!-- 阿黎是谁、在哪干活：她在**电脑上**执行，手机只是入口。
         这一行常驻——理解这个界面最重要的一件事就是这个。 -->
    <view class="ali-strip">
      <view class="ali-dot" />
      <text class="ali-name">阿黎</text>
      <text class="ali-desc">在电脑上执行 · 工具 / 工作区 / 派 agent 都在桌面端</text>
    </view>

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
        <view class="hm-pill pri c-btn" @tap="goConnect"><text>扫码连接电脑</text></view>
        <view class="hm-pill solid c-btn" @tap="goSettings"><text>设置</text></view>
      </view>
    </view>

    <!-- 链路状态：连不上时对话区照常在，只是明确说清为什么发不出去 -->
    <view v-if="paired && link !== 'online'" class="link-bar">
      <view class="link-dot" />
      <text class="link-txt">{{ linkMsg || '正在连电脑…' }}</text>
      <!-- 凭据被中转拒绝是终态（不再自动重连），必须给一条出路：
           直达设备管理，移除后重新扫码。 -->
      <view v-if="linkRevoked" class="link-act" @tap="goSettings"><text>去处理</text></view>
    </view>

    <!-- 运行胶囊：抄 dsh-mobile 的 ColorOS 流体云「胶囊态」——任务在跑时
         顶部常驻一小条，切走再回来看得见状态（真·流体云需原生 UTS 插件，这里先做应用内近似）。 -->
    <view v-if="paired && chat.turnActive" class="capsule">
      <view class="cap-dot" />
      <text class="cap-txt">任务运行中 · {{ fmtDur(elapsed) }}</text>
    </view>

    <!-- 上下文仪表：抄对方「上下文占用持久条」——模型窗口用掉几成，一眼可见 -->
    <view v-if="chat.context" class="ctx">
      <view class="ctx-bar">
        <view
          class="ctx-fill"
          :class="{ over: chat.context.percent >= 80 }"
          :style="{ width: Math.min(100, chat.context.percent) + '%' }"
        />
      </view>
      <text class="ctx-t">{{ fmtCtx(chat.context) }}</text>
    </view>

    <!-- 待办列表：抄对方 To-dos 卡片——agent 的计划步骤实时跟进 -->
    <view v-if="chat.todos.length" class="todos">
      <text class="todos-h">待办</text>
      <view
        v-for="t in chat.todos"
        :key="t.id"
        class="todo"
        :class="['st-' + t.status, t.status === 'completed' || t.status === 'done' ? 'done' : '']"
      >
        <text class="todo-ck">{{ todoIcon(t.status) }}</text>
        <text class="todo-t">{{ t.title }}</text>
      </view>
    </view>

    <hm-empty
      v-else-if="isEmpty"
      :title="source === 'ali' ? '和阿黎聊聊' : '还没有对话'"
      :hint="
        source === 'ali'
          ? '她直接在手机上回你，不经过电脑——电脑关机也能用。'
          : '在下面输入一句话下任务。任务实际在你的电脑上跑，这里只做入口与呈现。'
      "
    >
      <image class="mascot" src="/static/mascot.png" mode="aspectFit" />
    </hm-empty>

    <view v-for="m in messages" :key="m.id" :class="['bub', m.role, m.sending ? 'sending' : '']">
      <hm-step v-if="m.role === 'assistant'" :running="m.runningTools ?? []" :done="m.doneTools ?? []" />
      <hm-think v-if="m.role === 'assistant' && m.thinking" :text="m.thinking" />
      <rich-text v-if="m.role === 'assistant'" class="txt md" :nodes="renderMarkdown(m.text)" />
      <text v-else class="txt">{{ m.text }}</text>
      <text v-if="m.viaVoice" class="via">语音 · 长按可改</text>
      <!-- 助手气泡操作条：窄屏单行、横向滚动（抄对方窄屏顺序+横滚）。复制即时可用；时长是这轮跑了多久。 -->
      <view v-if="m.role === 'assistant'" class="acts">
        <view class="act" @tap="copy(m.text)"><text>复制</text></view>
        <text v-if="m.duration != null" class="dur">{{ fmtDur(m.duration) }}</text>
      </view>
    </view>

    <view v-if="recording" class="bub user live">
      <text class="txt">{{ partial || '听着呢…' }}</text>
    </view>

    <!-- 流式中的助手气泡：单独一个壳，收尾时转成正式气泡 -->
    <view v-if="streaming" class="bub assistant">
      <hm-step :running="chat.runningTools" :done="chat.doneTools" />
      <hm-think v-if="chat.thinking" :text="chat.thinking" />
      <!-- 逐字打字机：稳定段 + 最新字符（带淡入）+ 光标。落定后由下方正式气泡渲染 markdown。 -->
      <text class="txt typing">{{ typedText.slice(0, -1) }}</text><text
        v-if="typedText.length"
        :key="typedText.length"
        class="txt typing fadin"
      >{{ typedText.slice(-1) }}</text><text class="caret">▍</text>
    </view>

    <view v-if="chat.busy" class="hm-notice warn notice-line"><text>上一轮还在跑 · 你发的话会排队</text></view>
    <view v-else-if="chat.turnActive" class="hm-notice notice-line"><text>还在想 · 已运行 {{ fmtDur(elapsed) }}</text></view>
    <view v-if="chat.lastError" class="hm-notice err notice-line"><text>{{ chat.lastError }}</text></view>
    <view v-if="modelPushOff" class="hm-notice notice-line"><text>电脑端未启用模型配置推送 · 本地对话不可用</text></view>

    <view id="tail" class="tail" />

    <!-- 高危指令发前确认：对指令本身，不是工具审批 -->
    <view v-if="pendingRisky" class="risky">
      <text class="risky-t">确认发送这条？</text>
      <text class="risky-b">{{ pendingRisky.text }}</text>
      <view class="risky-keys">
        <view class="hm-pill pri key" @tap="confirmRisky"><text>发送</text></view>
        <view class="hm-pill soft key" @tap="pendingRisky = null"><text>取消</text></view>
      </view>
    </view>

    <!-- 工具审批：PC 停在等这一下，和对话在同一处，不另开页面 -->
    <view v-if="chat.pendingApproval" class="risky approval">
      <text class="risky-t">电脑要用「{{ chat.pendingApproval.tool }}」</text>
      <text class="risky-b">
        点了之后任务会按你的答复继续往下跑。
        <text v-if="!chat.pendingApproval.canPersist">永久放行只能在电脑上操作。</text>
      </text>
      <view class="risky-keys">
        <view class="hm-pill pri key" @tap="answerApproval('approved')"><text>允许一次</text></view>
        <view class="hm-pill soft key" @tap="answerApproval('denied')"><text>拒绝</text></view>
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
/* 终态（凭据被拒）横幅的出路按钮：暖色小药丸，与状态点同一行尾。 */
.link-act {
  flex: none;
  margin-left: auto;
  padding: 8rpx 24rpx;
  border-radius: $r-pill;
  background: $warn-bg;
}
.link-act text {
  font-size: $fs-mini;
  color: $warn;
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

/* 打字机：流式文本逐字显示。稳定段与最新字符都走 .txt 排版；最新字符带淡入，
   末尾一个终端式光标。落定后气泡切回 markdown，不再用这套。 */
.typing {
  display: inline;
  white-space: pre-wrap;
}
.fadin {
  animation: charIn 0.16s ease-out;
}
@keyframes charIn {
  from { opacity: 0.25; }
  to { opacity: 1; }
}
.caret {
  display: inline;
  color: $accent;
  font-weight: 600;
  animation: blink 1s step-end infinite;
}
@keyframes blink {
  0%, 100% { opacity: 1; }
  50% { opacity: 0; }
}

/* 运行胶囊（近似 ColorOS 流体云胶囊态）：顶部常驻、单个、圆角药丸，靠底色差分界。 */
.capsule {
  display: flex;
  align-items: center;
  gap: 12rpx;
  margin: 16rpx $gutter 0;
  padding: 12rpx 24rpx;
  background: $card;
  border-radius: $r-pill;
  align-self: flex-start;
}
.cap-dot {
  flex: none;
  width: 14rpx;
  height: 14rpx;
  border-radius: 50%;
  background: $ok;
  animation: pulse 1.4s ease-in-out infinite;
}
.cap-txt {
  font-size: $fs-mini;
  color: $fg2;
}

/* 阿黎身份条：常驻一行，说清"她在哪干活"。不是装饰，是理解这个界面的前提。 */
.ali-strip {
  display: flex;
  align-items: center;
  gap: 10rpx;
  margin: 8rpx $gutter 2rpx;
  padding: 14rpx 20rpx;
  background: $card;
  border-radius: $r-pill;
}
.ali-dot {
  flex: none;
  width: 14rpx;
  height: 14rpx;
  border-radius: 50%;
  background: $accent;
}
.ali-name {
  flex: none;
  font-size: $fs-sub;
  font-weight: 600;
  color: $fg;
}
.ali-desc {
  flex: 1;
  min-width: 0;
  font-size: $fs-mini;
  color: $fg2;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

/* 上下文仪表（抄对方「上下文占用持久条」）：细条 + 右侧文案，超过阈值转暖色。 */
.ctx {
  display: flex;
  align-items: center;
  gap: 14rpx;
  margin: 12rpx $gutter 0;
}
.ctx-bar {
  flex: 1;
  min-width: 0;
  height: 10rpx;
  border-radius: $r-pill;
  background: $raised;
  overflow: hidden;
}
.ctx-fill {
  height: 100%;
  border-radius: $r-pill;
  background: $accent;
  transition: width 0.4s ease-out;
}
.ctx-fill.over {
  background: $warn;
}
.ctx-t {
  flex: none;
  font-size: $fs-mini;
  color: $fg4;
}

/* 待办列表（抄对方 To-dos 卡片）：组内步骤，完成态打勾、在跑态高亮。 */
.todos {
  margin: 14rpx $gutter 0;
  padding: 18rpx 20rpx;
  background: $card;
  border-radius: $r-card;
}
.todos-h {
  display: block;
  font-size: $fs-mini;
  color: $fg2;
  margin-bottom: 10rpx;
}
.todo {
  display: flex;
  align-items: center;
  gap: 14rpx;
  padding: 8rpx 0;
}
.todo-ck {
  flex: none;
  width: 32rpx;
  text-align: center;
  font-size: 24rpx;
  color: $fg4;
}
.todo.done .todo-ck {
  color: $ok;
}
.todo.st-in_progress .todo-ck {
  color: $accent;
}
.todo-t {
  flex: 1;
  min-width: 0;
  font-size: $fs-sub;
  color: $fg;
}
.todo.done .todo-t {
  color: $fg4;
  text-decoration: line-through;
}

/* 助手气泡操作条：窄屏单行、横向滚动（抄对方窄屏顺序+横滚）。 */
.acts {
  display: flex;
  align-items: center;
  gap: 12rpx;
  margin-top: 12rpx;
  overflow-x: auto;
  -webkit-overflow-scrolling: touch;
}
.act {
  flex: none;
  padding: 10rpx 24rpx;
  border-radius: $r-pill;
  background: $raised;
  color: $fg2;
  font-size: $fs-mini;
}
.act:active {
  opacity: 0.7;
}
.dur {
  flex: none;
  margin-left: auto;
  font-size: $fs-mini;
  color: $fg4;
}

/* 发送中脉冲：用户消息右下角一个小圆点，收到回复后消失。 */
.bub.user.sending::after {
  content: '';
  position: absolute;
  bottom: 14rpx;
  right: 14rpx;
  width: 10rpx;
  height: 10rpx;
  border-radius: 50%;
  background: $fg4;
  animation: pulse 1.4s ease-in-out infinite;
}
.bub.user.sending {
  position: relative;
}
@keyframes pulse {
  0%, 100% { opacity: 1; transform: scale(1); }
  50% { opacity: 0.3; transform: scale(0.7); }
}

/* 提示条只留边距，形态见 styles/shared.scss 的 .hm-notice。 */
.notice-line {
  margin: 0 $gutter 20rpx;
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

/* 顶栏按钮：历史列表 + 通话 + 新建对话 */
:deep(.hist-btn),
:deep(.call-btn),
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
:deep(.call-btn:active),
:deep(.new-btn:active) {
  opacity: 0.7;
}
</style>
