<script setup lang="ts">
/**
 * 通话模式（第五个模式，不是第五个屏——能从通话掉回对话屏）
 *
 * 姿态：**眼睛忙**。与屏 2 的"手在忙、眼睛看着屏"是两种不同用法。
 *
 * **语音只做「发起」，不做「授权」**：通话 = 用户不看屏，在不看屏时要求他做
 * 安全决策本身是矛盾。所以这里所有审批一律退化为视觉确认 + 语音只播报状态
 * （"要做删除操作，请看一眼屏幕"）。这一条比 `2026-09-30` 的结论更强一步。
 *
 * 打断：调 `POST /v1/sessions/{sid}/interrupt`（P1 已落地）。但**已播出的音频
 * 收不回来**，所以打断后必须有一句确认，否则用户不知道自己有没有打断成功。
 *
 * 掉线：文字链路断了能重连重试，通话断了是一段对话直接蒸发。断掉的轮次要
 * 显式标成中断（`GET /v1/tasks` 的未跑完提示就是干这个的）。
 */
import { ref, onUnmounted } from 'vue'

type Turn = 'you' | 'agent' | 'silence'

const turn = ref<Turn>('agent')
const elapsed = ref(252) // 已通话秒数
const transcript = ref<string[]>(['它：read_file 那批里有个路径写错了…'])
let tick: number | undefined

function startTicker() {
  tick = window.setInterval(() => {
    elapsed.value += 1
  }, 1000)
}
onUnmounted(() => {
  if (tick) clearInterval(tick)
})

async function bargeIn() {
  turn.value = 'you'
  // TODO: 先中断当前轮（POST /v1/sessions/{sid}/interrupt），
  // 再把本轮的识别结果作为新 user 消息发出去。
  // 关键：**打断后要给一句确认**（"好，我明白了"），否则用户不知道打断是否生效。
  transcript.value.push('你：等一下，先别动那个文件')
}

function hangUp() {
  // TODO: 关闭录音与 TTS，会话状态回 idle
  uni.navigateBack()
}
</script>

<template>
  <view class="screen">
    <view class="appbar">
      <text class="title">通话中 · {{ Math.floor(elapsed / 60) }}:{{ String(elapsed % 60).padStart(2, '0') }}</text>
    </view>

    <view class="center">
      <text class="turn">{{ turn === 'you' ? '你在说' : turn === 'agent' ? '它正在说' : '沉默' }}</text>
      <view class="wave">
        <view v-for="i in 18" :key="i" class="bar" :style="{ height: (10 + ((i * 37) % 26)) + 'rpx' }" />
      </view>

      <view class="card">
        <text v-for="(l, i) in transcript" :key="i" class="line">{{ l }}</text>
      </view>
    </view>

    <view class="notice warn">
      <text>要做删除操作，请看一眼屏幕 —— 通话中所有审批都需要视觉确认</text>
    </view>

    <view class="row">
      <view class="btn" @tap="bargeIn"><text class="bl">打断</text></view>
      <view class="btn end" @tap="hangUp"><text class="bl">挂断</text></view>
      <view class="btn"><text class="bl">转写</text></view>
    </view>
  </view>
</template>

<style lang="scss" scoped>
.screen { display: flex; flex-direction: column; height: 100vh; background: $paper; }
.appbar { padding: 100rpx 30rpx 20rpx; background: $base; border-bottom: 1rpx solid $line; }
.title { font-size: 26rpx; color: $fg2; }

.center { flex: 1; display: flex; flex-direction: column; align-items: center; padding: 60rpx 40rpx; }
.turn { font-size: 22rpx; color: $fg2; }
.wave { display: flex; align-items: center; gap: 6rpx; height: 80rpx; margin: 30rpx 0 50rpx; }
.bar { width: 6rpx; background: $accent; border-radius: 3rpx; }

.card { width: 100%; background: $card; border: 1rpx solid $line; border-radius: $r-card; padding: 22rpx; }
.line { display: block; font-size: 21rpx; color: $fg2; line-height: 1.55; font-family: monospace; }

.notice { margin: 0 26rpx 20rpx; border-radius: 20rpx; padding: 20rpx; font-size: 21rpx; line-height: 1.5; }
.notice.warn { background: $warn-bg; color: $warn; border: 1rpx solid rgba(211, 163, 76, 0.25); }

.row { display: flex; gap: 16rpx; padding: 0 26rpx 40rpx; }
.btn {
  flex: 1; height: 96rpx; line-height: 96rpx; text-align: center;
  border-radius: 48rpx; background: $card; border: 1rpx solid $line; color: $fg2;
}
.btn.end { background: $err-bg; border-color: rgba(224, 107, 82, 0.3); color: $err; }
.bl { font-size: 22rpx; }
</style>
