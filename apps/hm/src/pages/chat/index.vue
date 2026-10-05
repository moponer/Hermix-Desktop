<script setup lang="ts">
/**
 * 屏 2 · 对话
 *
 * 与屏 3（审批）、屏 4（任务）的分界：**这里是"文档模型"**——可回滚的消息流。
 * 语音通话（屏 5）是另一种心智模型，能从通话掉回这里。
 *
 * 语音发送消息（长按说话）在这一屏，但**不做"说完自动发"**：ASR 误识别在
 * 没有视觉确认时是单向的，长按-松发给了一个人工的确认动作。识别结果回显
 * 成用户气泡（错了能改），命中高危动词再弹一次发前确认。
 */
import { ref, computed } from 'vue'
import { isRiskyInstruction } from '../../core/voice'
import { dispatch, reduce, initialChatState, type ChatState } from '../../core/frames'
import type { AsrProvider } from '../../core/voice'

interface Bubble {
  id: string
  role: 'user' | 'assistant'
  text: string
  /** 语音发送的消息标记：这条是识别出来的，不是打字的 */
  viaVoice?: boolean
  /** 工具轨迹：正在跑 / 已跑完的工具名 */
  running?: string[]
  doneTools?: string[]
}

const messages = ref<Bubble[]>([
  { id: '1', role: 'user', text: '把 tools 那一层的 dispatch 拆出来，先给我看结构' },
  { id: '2', role: 'assistant', text: '入口在 tool_dispatch.rs，但它把「选工具」和「执行工具」耦在一起了。', running: ['bash'], doneTools: ['read_file', 'grep'] },
])

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
}

let asr: AsrProvider | null = null

function attachProvider(p: AsrProvider | null): void {
  asr = p
}

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
  if (isRiskyInstruction(text)) {
    // 对**指令本身**的确认，与工具审批是两件事
    pendingRisky.value = { text }
  }
}

function confirmRisky() {
  if (pendingRisky.value) sendText(pendingRisky.value.text)
  pendingRisky.value = null
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
}
</script>

<template>
  <view class="screen">
    <view class="appbar">
      <text class="title">重构那批工具</text>
    </view>

    <scroll-view class="body" scroll-y>
      <view v-for="m in messages" :key="m.id" :class="['bub', m.role]">
        <view v-if="m.running?.length || m.doneTools?.length" class="trace">
          <text v-for="d in (m.doneTools ?? [])" :key="d" class="t-done">● {{ d }}</text>
          <text v-for="r in (m.running ?? [])" :key="r" class="t-run">● {{ r }}</text>
        </view>
        <text class="txt">{{ m.text }}</text>
        <text v-if="m.viaVoice" class="via">语音 · 长按可改</text>
      </view>

      <view v-if="recording" class="bubble-live">
        <text class="dim">{{ partial || '听着呢…' }}</text>
      </view>

      <!-- 流式中的助手气泡：单独一个壳，收尾时转成正式气泡 -->
      <view v-if="streaming" class="bub assistant">
        <view v-if="chat.runningTools.length || chat.doneTools.length" class="trace">
          <text v-for="d in chat.doneTools" :key="d" class="t-done">● {{ d }}</text>
          <text v-for="r in chat.runningTools" :key="r" class="t-run">● {{ r }}</text>
        </view>
        <text class="txt">{{ streaming.text }}</text>
      </view>

      <view v-if="chat.busy" class="notice warn">
        <text>上一轮还在跑 · 你发的话会排队</text>
      </view>
      <view v-else-if="chat.turnActive" class="notice">
        <text>还在想 · 下一条心跳 60s 后</text>
      </view>
      <view v-if="chat.lastError" class="notice err">
        <text>{{ chat.lastError }}</text>
      </view>
    </scroll-view>

    <!-- 高危指令发前确认：对指令本身，不是工具审批 -->
    <view v-if="pendingRisky" class="risky">
      <view class="risky-txt">
        <text class="risky-t">确认发送这条？</text>
        <text class="risky-b">{{ pendingRisky.text }}</text>
      </view>
      <view class="risky-keys">
        <button class="btn pri sm" @tap="confirmRisky">发送</button>
        <button class="btn sec sm" @tap="pendingRisky = null">取消</button>
      </view>
    </view>

    <view class="composer">
      <text v-if="chat.turnActive" class="hint">运行中 · 你发的话会排队</text>
      <view class="field">
        <input v-model="draft" class="inp" placeholder="说点什么…" confirm-type="send" @confirm="send" />
        <view class="mic" :class="{ on: recording }" @longpress="onLongPressStart" @touchend="onLongPressEnd">
          <text>{{ recording ? '松开发送' : '长按说话' }}</text>
        </view>
        <view class="send" @tap="send">↑</view>
      </view>
    </view>
  </view>
</template>

<style lang="scss" scoped>
.screen { display: flex; flex-direction: column; height: 100vh; background: $paper; }
.appbar { padding: 100rpx 30rpx 20rpx; background: $base; border-bottom: 1rpx solid $line; }
.title { font-size: 28rpx; font-weight: 600; color: $fg; }

.body { flex: 1; padding: 24rpx 26rpx; }

.bub {
  max-width: 84%;
  padding: 18rpx 22rpx;
  border-radius: $r-bubble;
  margin-bottom: 20rpx;
  font-size: 26rpx;
  line-height: 1.55;
  box-sizing: border-box;
}
.bub.user { margin-left: auto; background: $raised; color: $fg; border-bottom-right-radius: 10rpx; }
.bub.assistant { background: $card; border: 1rpx solid $line; border-bottom-left-radius: 10rpx; }
.bubble-live {
  align-self: flex-end;
  max-width: 84%;
  background: $raised;
  border: 1rpx dashed $edge;
  border-radius: $r-bubble;
  padding: 18rpx 22rpx;
  margin-bottom: 20rpx;
  box-sizing: border-box;
}
.dim { color: $fg2; font-size: 24rpx; }

.trace { display: flex; flex-wrap: wrap; gap: 14rpx; margin-bottom: 12rpx; font-size: 19rpx; }
.t-done { color: $ok; }
.t-run { color: $warn; }
.via { display: block; margin-top: 8rpx; font-size: 18rpx; color: $fg4; }

.notice { font-size: 21rpx; color: $fg2; padding: 16rpx; }

.risky {
  border-top: 1rpx solid $line;
  background: $warn-bg;
  padding: 22rpx 26rpx;
}
.risky-txt { display: flex; flex-direction: column; gap: 6rpx; margin-bottom: 18rpx; }
.risky-t { font-size: 24rpx; font-weight: 600; color: $warn; }
.risky-b { font-size: 22rpx; color: $fg2; }
.risky-keys { display: flex; gap: 14rpx; }
.btn { flex: 1; height: 72rpx; line-height: 72rpx; border-radius: 36rpx; font-size: 24rpx; border: none; padding: 0; }
.btn.pri { background: $accent; color: $on-accent; }
.btn.sec { background: transparent; border: 1rpx solid $edge; color: $fg2; }

.composer { border-top: 1rpx solid $line; padding: 16rpx 22rpx 30rpx; }
.hint { display: block; font-size: 20rpx; color: $fg2; margin-bottom: 12rpx; }
.field {
  display: flex;
  align-items: center;
  height: 76rpx;
  background: $card;
  border: 1rpx solid $edge;
  border-radius: 38rpx;
  padding: 0 10rpx 0 26rpx;
}
.inp { flex: 1; color: $fg; font-size: 26rpx; }
.mic {
  padding: 0 20rpx;
  height: 60rpx;
  line-height: 60rpx;
  border-radius: 30rpx;
  background: $raised;
  color: $fg2;
  font-size: 20rpx;
}
.mic.on { background: $accent; color: $on-accent; }
.send {
  width: 60rpx;
  height: 60rpx;
  line-height: 56rpx;
  text-align: center;
  border-radius: 30rpx;
  background: $accent;
  color: $on-accent;
  margin-left: 10rpx;
}
</style>
