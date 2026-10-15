<script setup lang="ts">
/**
 * 通话页 —— 第五个**模式**，不是另一条消息流。
 *
 * 为什么是「另一个模式」而不是「另一种消息展示」：这是「边走边决策」的
 * 交互形态。两套不同的「说话者」显示方式并存时，用户无法分辨是谁在说，
 * 通话恰好是听力通道唯一被占用的场景。
 *
 * **「要删除、要看一眼屏幕」——通话中 = 用户在开车/走路时要决策**，这是安全
 * 强约束，见 `2026-09-30` 的一次强制会审结论。
 *
 * **普通呼叫不打断正在进行的通话**；插话（barge-in）= 用户明确「要说话」的
 * 自然行为，是强意图。服务端的**插话是硬打断**（`POST /v1/sessions/{sid}/interrupt`），
 * 但**打断之后必须在屏幕上给出明确的话**，否则用户不知道这件事是不是真的发生了。
 *
 * ## 待完成（全端 WebRTC）
 *   1. 实时转写：relay 需要暴露 WebRTC 信令端点（SDP offer/answer），
 *      手机端通过 `POST /v1/sessions/{sid}/webrtc/offer` 获取 answer，
 *      然后建立 P2P 音频流。转写结果通过 WebRTC data-channel 或
 *      轮询 `GET /v1/sessions/{sid}/transcript` 推送到手机端。
 *   2. TTS 打断：同样通过 WebRTC outbound audio track 实现。
 *   当前为**占位实现**：计时器正常，transcript 显示假数据，待全端 WebRTC
 *   信令就绪后再接通。
 */
import { ref, onMounted, onUnmounted } from 'vue'
import { interruptSession } from '../../core/tasks'

type Turn = 'you' | 'agent' | 'silence'

/** 当前通话的 sid（从页面参数传入）。 */
const currentSid = ref('')
const turn = ref<Turn>('silence')
const elapsed = ref(0)
/** 转写文本（实时追加）；手机端 WebRTC 就绪后由 data-channel 推送）。 */
const transcript = ref<string[]>([])

let tick: ReturnType<typeof setInterval> | undefined
onMounted(() => {
  tick = setInterval(() => {
    elapsed.value += 1
  }, 1000)
})
onUnmounted(() => {
  if (tick) clearInterval(tick)
})

const clock = () =>
  `${Math.floor(elapsed.value / 60)}:${String(elapsed.value % 60).padStart(2, '0')}`

/** 波形高度：静态伪随机，断开时是静止的一排小柱，接通后随「它正在说」起伏。 */
const waveH = (i: number) => 10 + ((i * 37) % 26)

async function bargeIn() {
  if (!currentSid.value) return
  const res = await interruptSession(currentSid.value)
  turn.value = 'you'
  transcript.value.push('【打断】')
  if (!res.interrupted) {
    uni.showToast({ title: res.reason ?? '打断失败', icon: 'none' })
  }
}

function hangUp() {
  // TODO（全端 WebRTC）：关闭录音 track，TTS 停，状态置 idle
  uni.navigateBack()
}
</script>

<template>
  <hm-shell title="通话中">
    <template #end><text class="clock">{{ clock() }}</text></template>

    <view class="center">
      <text class="turn">{{ turn === 'you' ? '你在说' : turn === 'agent' ? '它正在说' : '沉默' }}</text>
      <view class="wave" :class="{ live: turn === 'agent' }">
        <view v-for="i in 18" :key="i" class="bar" :style="{ height: waveH(i) + 'rpx' }" />
      </view>

      <view class="card">
        <text v-for="(l, i) in transcript" :key="i" class="line">{{ l }}</text>
        <text v-if="!transcript.length" class="line dim">还没有转写内容。</text>
      </view>
    </view>

    <view class="hm-notice warn notice">
      <text>要做删除操作，请看一眼屏幕 —— 通话中所有审批都需要视觉确认</text>
    </view>

    <template #bottom>
      <view class="row">
        <view class="hm-pill ghost btn" @tap="bargeIn"><text class="bl">打断</text></view>
        <view class="hm-pill danger btn" @tap="hangUp"><text class="bl">挂断</text></view>
      </view>
    </template>
  </hm-shell>
</template>

<style lang="scss" scoped>
.clock {
  font-size: $fs-sub;
  color: $fg3;
  font-family: monospace;
}

.center {
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: 70rpx 40rpx;
}
.turn {
  font-size: $fs-sub;
  color: $fg2;
}
.wave {
  display: flex;
  align-items: center;
  gap: 6rpx;
  height: 80rpx;
  margin: 30rpx 0 46rpx;
}
/* 静态就是静止的：通话里那套「两个人显示方式必须一致」的原则，
   波形只有状态变化（说话/沉默），不做无意义的持续动画。 */
.bar {
  width: 6rpx;
  background: $accent;
  border-radius: 3rpx;
  opacity: 0.3;
}
.wave.live .bar {
  animation: talk 1.1s ease-in-out infinite;
}
.wave.live .bar:nth-child(3n) {
  animation-delay: 0.18s;
}
.wave.live .bar:nth-child(3n + 1) {
  animation-delay: 0.36s;
}
@keyframes talk {
  0%,
  100% {
    transform: scaleY(0.35);
    opacity: 0.45;
  }
  50% {
    transform: scaleY(1);
    opacity: 1;
  }
}

.card {
  width: 100%;
  background: $card;
  border-radius: $r-card;
  padding: 22rpx;
}
.line {
  display: block;
  font-size: $fs-mini;
  color: $fg2;
  line-height: 1.55;
  font-family: monospace;
}
.line.dim {
  color: $fg4;
}

.notice {
  margin: 0 $gutter;
  padding: 20rpx;
}

.row {
  display: flex;
  gap: 16rpx;
  padding: 0 $gutter;
}
.btn {
  flex: 1;
  height: 108rpx;
  border-radius: 54rpx;
}
.bl {
  font-size: $fs-body;
  font-weight: 500;
}
</style>
