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
import { isPaired } from '../../core/pairing'
import { relayBaseUrl } from '../../core/clock'
import { ensureSplashClosed } from '../../core/boot'
import { ChatSession } from '../../core/session'
import { renderMarkdown } from '../../core/markdown'
import { newSessionId, loadSession, saveSession } from '../../core/history'
import { interruptSession } from '../../core/tasks'
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
  saveSession(currentSid.value, messages.value)
}


// ── 会话连接 ──────────────────────────────────────────────────────
//
// 链路状态是**主界面上的一个状态**，不是拦截层：连不上时对话区照常在，
// 只是发不出去并且说清楚为什么。早期版本把「没配对」做成整页拦截，
// 那是把「不能做什么」和「进不来」混成一件事。
const link = ref<'offline' | 'connecting' | 'online'>('offline')
const linkMsg = ref('')
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
    },
  })
  await session.start()
}

// 页面可能被反复进入（reLaunch 回来、tab 切换），onShow 里 start 是幂等的，
// 不会叠连接。
onShow(() => {
  paired.value = isPaired()
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
  const next = reduce(chat.value, dispatch(raw))
  // 桌面开始处理的第一帧：清除所有用户消息的 sending 状态
  if (!prevTurnActive && next.turnActive) {
    messages.value.forEach((m) => { m.sending = false })
  }
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
        saveSession(currentSid.value, messages.value)
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
  void (async () => {
    // 等连接真正就绪（hello + key.exchange 完成）再发，否则 send() 返回 false
    // 导致消息「发出去没回」而用户以为是网络慢。
    await ensureSession()
    // 先发出去，成功了才落气泡：连不上时留一条永远等不到回复的假消息，
    // 比直接不落更糟——用户会以为电脑收到了。
    const ok = (await session?.send(text)) ?? false
    if (!ok) {
      uni.showToast({ title: linkMsg.value || '还没连上电脑，正在重连', icon: 'none' })
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
        <view class="hm-pill pri c-btn" @tap="goConnect"><text>扫码连接电脑</text></view>
        <view class="hm-pill solid c-btn" @tap="goSettings"><text>设置</text></view>
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

    <view v-for="m in messages" :key="m.id" :class="['bub', m.role, m.sending ? 'sending' : '']">
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

    <view v-if="chat.busy" class="hm-notice warn notice-line"><text>上一轮还在跑 · 你发的话会排队</text></view>
    <view v-else-if="chat.turnActive" class="hm-notice notice-line"><text>还在想 · 下一条心跳 60s 后</text></view>
    <view v-if="chat.lastError" class="hm-notice err notice-line"><text>{{ chat.lastError }}</text></view>

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
