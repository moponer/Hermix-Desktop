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
import { ref, computed, nextTick } from 'vue'
import { onUnmounted } from 'vue'
import { onReady } from '@dcloudio/uni-app'
import { isRiskyInstruction } from '../../core/voice'
import { dispatch, reduce, initialChatState, type ChatState } from '../../core/frames'
import { isPaired } from '../../core/pairing'
import { relayBaseUrl } from '../../core/clock'
import { ensureSplashClosed } from '../../core/boot'
import type { AsrProvider } from '../../core/voice'

interface Bubble {
  id: string
  role: 'user' | 'assistant'
  text: string
  /** 语音发送的消息标记：这条是识别出来的，不是打字的 */
  viaVoice?: boolean
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
  const prev = chat.value.streamingId
  const next = reduce(chat.value, dispatch(raw))
  chat.value = next
  // 一轮落定：把流式气泡转成正式气泡
  if (prev && next.streamingId && next.turnActive === false) {
    messages.value.push({ id: prev, role: 'assistant', text: next.streamingText })
    chat.value = { ...next, streamingId: null, streamingText: '' }
  }
  scrollToTail()
}

let asr: AsrProvider | null = null

async function onLongPressStart() {
  if (!asr?.enabled) {
    // 没配 ASR 就不假装能听：明确说，而不是按住没反应
    uni.showToast({ title: '未配置语音识别', icon: 'none' })
    return
  }
  recording.value = true
  partial.value = ''
  asr.onPartial((t) => {
    partial.value = t
  })
  await asr.start()
}

async function onLongPressEnd() {
  if (!asr || !recording.value) return
  recording.value = false
  // 录音块由平台 recorder 逐块推给 asr.push —— 接入点见文件末 TODO
  const result = await asr.stop()
  const text = (result.final ?? '').trim()
  partial.value = ''
  if (!text) {
    uni.showToast({ title: '没听清', icon: 'none' })
    return
  }
  // 回显成用户气泡（还没发出去）
  messages.value.push({ id: `v${Date.now()}`, role: 'user', text, viaVoice: true })
  scrollToTail()
  if (isRiskyInstruction(text)) {
    // 对**指令本身**的确认，与工具审批是两件事
    pendingRisky.value = { text }
  }
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

function send() {
  sendText(draft.value)
  draft.value = ''
}

function sendText(t: string) {
  const text = t.trim()
  if (!text) return
  // 打字发的补一条（语音已经回显过气泡了）
  if (!messages.value.some((m) => m.role === 'user' && m.text === text && m.viaVoice)) {
    messages.value.push({ id: String(Date.now()), role: 'user', text })
  }
  // TODO(接 relayClient): send(JSON.stringify({ type: 'user.message', content: text }))
  //   turnActive / 工具轨迹 / 三态回执都从回来的帧里推（见 onFrame），
  //   **不在这里自己置 true** —— 那样"发出去"和"服务端开始跑"就分不开了。
  // TODO(接麦克风): longpress 期间用 uni.getRecorderManager() 逐块调
  //   asr.push(pcm)
  scrollToTail()
}

/**
 * 中断。只有 `turnActive` 为真时这个按钮才出现——那表示**服务端确实在跑**，
 * 所以它不是常驻的摆设按钮。
 */
function stop() {
  // TODO(接 REST): POST /v1/sessions/{sid}/interrupt
  //   桌面端已经在 2026-09-30 起提供该端点；缺的是手机侧拿到 sid 的路径。
  //   中断后**必须**回一句明确的话（PC 侧已终止），否则用户不知道点了没有。
  uni.showToast({ title: '中断还没接上', icon: 'none' })
}

onUnmounted(() => {
  draft.value = ''
})
</script>

<template>
  <hm-shell title="对话" :scroll-into-view="anchor">
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

    <hm-empty
      v-else-if="isEmpty"
      title="还没有对话"
      hint="在下面输入一句话下任务。任务实际在你的电脑上跑，这里只做入口与呈现。"
    >
      <image class="mascot" src="/static/mascot.png" mode="aspectFit" />
    </hm-empty>

    <view v-for="m in messages" :key="m.id" :class="['bub', m.role]">
      <hm-step v-if="m.role === 'assistant'" :running="[]" :done="[]" />
      <text class="txt">{{ m.text }}</text>
      <text v-if="m.viaVoice" class="via">语音 · 长按可改</text>
    </view>

    <view v-if="recording" class="bub user live">
      <text class="txt">{{ partial || '听着呢…' }}</text>
    </view>

    <!-- 流式中的助手气泡：单独一个壳，收尾时转成正式气泡 -->
    <view v-if="streaming" class="bub assistant">
      <hm-step :running="chat.runningTools" :done="chat.doneTools" />
      <text class="txt">{{ streaming.text }}</text>
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
  font-size: 28rpx;
  font-weight: 600;
  color: $fg;
}
.c-hint {
  display: block;
  margin-top: 12rpx;
  font-size: 22rpx;
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
  height: 76rpx;
  line-height: 76rpx;
  text-align: center;
  border-radius: 38rpx;
  background: $raised;
  color: $fg;
  font-size: 24rpx;
}
.c-btn.pri {
  background: $accent;
  color: $on-accent;
}

/* 气泡靠底色分层：用户=$raised（比页底高两档），助手=$card（高一档）。 */
.bub {
  max-width: 84%;
  padding: 18rpx 22rpx;
  border-radius: $r-bubble;
  margin: 0 26rpx 20rpx;
  font-size: 26rpx;
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
  font-size: 18rpx;
  color: $fg4;
}
.tail {
  height: 8rpx;
}

.notice {
  margin: 0 26rpx 20rpx;
  padding: 16rpx 20rpx;
  border-radius: 18rpx;
  font-size: 21rpx;
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
  margin: 0 26rpx 20rpx;
  background: $warn-bg;
  border-radius: 18rpx;
  padding: 20rpx 22rpx;
}
.risky-t {
  display: block;
  font-size: 23rpx;
  font-weight: 600;
  color: $warn;
}
.risky-b {
  display: block;
  margin-top: 6rpx;
  margin-bottom: 16rpx;
  font-size: 22rpx;
  color: $fg2;
}
.risky-keys {
  display: flex;
  gap: 14rpx;
}
.key {
  flex: 1;
  height: 70rpx;
  line-height: 70rpx;
  text-align: center;
  border-radius: 35rpx;
  font-size: 23rpx;
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
  font-size: 20rpx;
  color: $fg2;
  margin-bottom: 10rpx;
}
.hint.off {
  color: $fg4;
}
.field {
  display: flex;
  align-items: center;
  height: 80rpx;
  background: $card;
  border-radius: 40rpx;
  padding: 0 10rpx 0 24rpx;
}
.inp {
  flex: 1;
  min-width: 0;
  color: $fg;
  font-size: 26rpx;
}
.mic {
  flex: none;
  padding: 0 18rpx;
  height: 60rpx;
  line-height: 60rpx;
  border-radius: 30rpx;
  background: $raised;
  color: $fg2;
  font-size: 20rpx;
}
.mic.on {
  background: $accent;
  color: $on-accent;
}
.send {
  flex: none;
  width: 60rpx;
  height: 60rpx;
  line-height: 60rpx;
  text-align: center;
  border-radius: 30rpx;
  background: $accent;
  color: $on-accent;
  margin-left: 10rpx;
  font-size: 24rpx;
}
.send.stop {
  background: $err-bg;
  color: $err;
}
.send.off {
  opacity: 0.35;
}
</style>
