<script setup lang="ts">
/**
 * 屏 2 · 对话
 *
 * 与屏 3（审批）、屏 4（任务）的分界：**这里是"文档模型"**——可回滚的消息流。
 * 语音通话（屏 5）是另一种心智模型，能从通话掉回这里。
 *
 * 语音发送消息（长按说话）在这一屏，但**不做"说完自动发"**：ASR 误识别在
 * 没有视觉确认时是单向的，长按-松发给了一个人工的确认动作。
 */
import { ref } from 'vue'

interface Bubble {
  id: string
  role: 'user' | 'assistant'
  text: string
  /** 语音发送的消息标记：这条是识别出来的，不是打字的 */
  viaVoice?: boolean
  /** 工具轨迹：正在跑的工具名 */
  running?: string[]
  doneTools?: string[]
}

const messages = ref<Bubble[]>([
  { id: '1', role: 'user', text: '把 tools 那一层的 dispatch 拆出来，先给我看结构' },
  { id: '2', role: 'assistant', text: '入口在 tool_dispatch.rs，但它把「选工具」和「执行工具」耦在一起了。', running: ['bash'], doneTools: ['read_file', 'grep'] },
])

const draft = ref('')
const running = ref(false)
const recording = ref(false)

function send() {
  const t = draft.value.trim()
  if (!t) return
  messages.value.push({ id: String(Date.now()), role: 'user', text: t })
  draft.value = ''
  running.value = true
  // TODO(接流式 WS): 真实实现订阅 /v1/ws-stream 的增量帧，
  // 工具事件更新 running/doneTools，turn.done 收尾 + 三态回执
}

function onLongPressStart() {
  recording.value = true
  // TODO(接流式 ASR): 录音起流；识别中的增量文本实时上屏
}
function onLongPressEnd() {
  recording.value = false
  // TODO: 松手才发；识别结果作为用户气泡回显（错了能改），命中高危动词先确认
}
</script>

<template>
  <view class="screen">
    <view class="appbar">
      <text class="title">重构那批工具</text>
    </view>

    <scroll-view class="body" scroll-y :scroll-top="0">
      <view
        v-for="m in messages"
        :key="m.id"
        :class="['bub', m.role]"
      >
        <view v-if="m.running?.length || m.doneTools?.length" class="trace">
          <text v-for="d in (m.doneTools ?? [])" :key="d" class="t-done">● {{ d }}</text>
          <text v-for="r in (m.running ?? [])" :key="r" class="t-run">● {{ r }}</text>
        </view>
        <text class="txt">{{ m.text }}</text>
        <text v-if="m.viaVoice" class="via">语音</text>
      </view>
      <view v-if="running" class="notice">
        <text>还在想 · 下一条心跳 60s 后</text>
      </view>
    </scroll-view>

    <view class="composer">
      <text v-if="running" class="hint">运行中 · 你发的话会排队</text>
      <view class="field">
        <input v-model="draft" class="inp" placeholder="说点什么…" confirm-type="send" @confirm="send" />
        <view
          class="mic"
          :class="{ on: recording }"
          @longpress="onLongPressStart"
          @touchend="onLongPressEnd"
        >
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

.trace { display: flex; flex-wrap: wrap; gap: 14rpx; margin-bottom: 12rpx; font-size: 19rpx; }
.t-done { color: $ok; }
.t-run { color: $warn; }
.via { display: block; margin-top: 8rpx; font-size: 18rpx; color: $fg4; }

.notice { font-size: 21rpx; color: $fg2; padding: 16rpx; }

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
