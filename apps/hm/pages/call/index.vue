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
 * 改造重点：原版 `transcript` 与 `elapsed` 都是假数据（252 秒、一条假转写），
 * 且 `startTicker` **定义了却从没被调用**，计时器根本不转。`window.setInterval`
 * 在 App / 小程序端没有 `window`，一旦真被调用就是 ReferenceError。
 */
import { ref, onMounted, onUnmounted } from 'vue'

type Turn = 'you' | 'agent' | 'silence'

const turn = ref<Turn>('silence')
const elapsed = ref(0)
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
  turn.value = 'you'
  // TODO(接 REST): POST /v1/sessions/{sid}/interrupt
  // TODO(接 TTS): 打断后播报，**屏幕上也要出一句**——
  //   「我打断你一下」这种话是用户唯一能确认「电脑那边真的停了」的证据。
}

function hangUp() {
  // TODO(接 TTS): 关闭录音，TTS 停，会话状态置 idle
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

    <view class="notice warn">
      <text>要做删除操作，请看一眼屏幕 —— 通话中所有审批都需要视觉确认</text>
    </view>

    <template #bottom>
      <view class="row">
        <view class="btn" @tap="bargeIn"><text class="bl">打断</text></view>
        <view class="btn end" @tap="hangUp"><text class="bl">挂断</text></view>
      </view>
    </template>
  </hm-shell>
</template>

<style lang="scss" scoped>
.clock {
  font-size: 22rpx;
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
  font-size: 22rpx;
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
  font-size: 21rpx;
  color: $fg2;
  line-height: 1.55;
  font-family: monospace;
}
.line.dim {
  color: $fg4;
}

.notice {
  margin: 0 26rpx;
  background: $warn-bg;
  border-radius: 18rpx;
  padding: 20rpx;
  font-size: 21rpx;
  line-height: 1.5;
  color: $warn;
}

.row {
  display: flex;
  gap: 16rpx;
  padding: 0 26rpx;
}
.btn {
  flex: 1;
  height: 96rpx;
  line-height: 96rpx;
  text-align: center;
  border-radius: 48rpx;
  background: $card;
  color: $fg2;
}
.btn.end {
  background: $err-bg;
  color: $err;
}
.bl {
  font-size: 22rpx;
}
</style>
