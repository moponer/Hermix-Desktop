<script setup lang="ts">
/**
 * 步骤卡片：这一轮的工具轨迹。
 *
 * 参考 WorkBuddy 对话页的「拆解为 N 个步骤 + 展开 `>` + 完成态标签」。
 * 我们这边数据是一样的东西（`frames.ts` 的 runningTools / doneTools），
 * 只是默认**折叠**——手机上一步一行，全展开会把真正的回答顶到屏幕外。
 *
 * 「已完成」用绿色，「在跑」用主色：这两个颜色和任务列表、审批里的
 * 在线/离线标签是同一套语义色，不另造。
 */
import { ref, computed } from 'vue'

const props = defineProps<{ running: string[]; done: string[] }>()

const open = ref(false)
const total = computed(() => props.running.length + props.done.length)
const summary = computed(() => {
  if (props.running.length) return `正在跑 ${props.running[props.running.length - 1]}`
  if (props.done.length) return `${props.done.length} 步已完成`
  return ''
})
</script>

<template>
  <view v-if="total" class="step" @tap="open = !open">
    <view class="head">
      <view class="left">
        <text class="n">{{ total }} 个步骤</text>
        <text v-if="done.length" class="done-tag">已完成</text>
      </view>
      <text class="sum">{{ summary }}</text>
      <text class="caret">{{ open ? '⌄' : '›' }}</text>
    </view>
    <view v-if="open" class="list">
      <text v-for="d in props.done" :key="'d' + d" class="row done">✓ {{ d }}</text>
      <text v-for="r in props.running" :key="'r' + r" class="row run">● {{ r }}</text>
    </view>
  </view>
</template>

<style lang="scss" scoped>
.step {
  background: $card;
  border-radius: $r-card;
  padding: 18rpx 20rpx;
  margin-bottom: 16rpx;
}
.head {
  display: flex;
  align-items: center;
  gap: 12rpx;
}
.left {
  display: flex;
  align-items: center;
  gap: 10rpx;
  flex: none;
}
.n {
  font-size: 20rpx;
  color: $fg2;
}
.done-tag {
  font-size: 17rpx;
  color: $ok;
  background: rgba(132, 179, 131, 0.14);
  padding: 3rpx 10rpx;
  border-radius: $r-pill;
}
.sum {
  flex: 1;
  font-size: 19rpx;
  color: $fg4;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.caret {
  flex: none;
  color: $fg4;
  font-size: 26rpx;
}
.list {
  margin-top: 14rpx;
  display: flex;
  flex-direction: column;
  gap: 8rpx;
}
.row {
  font-size: 20rpx;
  font-family: monospace;
}
.done {
  color: $ok;
}
.run {
  color: $accent;
}
</style>
