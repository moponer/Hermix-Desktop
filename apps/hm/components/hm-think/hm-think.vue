<script setup lang="ts">
/**
 * 思考过程折叠块：抄 dsh-mobile 的「Think」折叠——模型推理过程默认收起，
 * 点开看全文。与桌面端同一个 <think> 解析源（PC 侧 StreamEvent::Thinking）。
 */
import { ref } from 'vue'

const props = defineProps<{ text: string }>()
const open = ref(false)
</script>

<template>
  <view class="think" :class="{ open }" @tap="open = !open">
    <view class="hd">
      <view class="dot" />
      <text class="t">思考过程</text>
      <text class="n">{{ (props.text || '').length }}</text>
      <text class="caret">{{ open ? '⌄' : '›' }}</text>
    </view>
    <view v-if="open" class="body">
      <text class="txt">{{ props.text }}</text>
    </view>
  </view>
</template>

<style lang="scss" scoped>
.think {
  background: $card;
  border-radius: $r-card;
  padding: 14rpx 18rpx;
  margin-bottom: 12rpx;
}
.hd {
  display: flex;
  align-items: center;
  gap: 10rpx;
}
.dot {
  flex: none;
  width: 12rpx;
  height: 12rpx;
  border-radius: 50%;
  background: $accent;
}
.t {
  flex: none;
  font-size: $fs-mini;
  color: $fg2;
}
.n {
  flex: none;
  font-size: $fs-mini;
  color: $fg4;
}
.caret {
  flex: none;
  margin-left: auto;
  color: $fg4;
  font-size: 26rpx;
}
.body {
  margin-top: 10rpx;
}
.txt {
  display: block;
  font-size: $fs-mini;
  line-height: 1.55;
  color: $fg3;
  white-space: pre-wrap;
  word-break: break-word;
}
</style>
