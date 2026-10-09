<script setup lang="ts">
/**
 * 空态。
 *
 * 改造前 chat / tasks / approval 三页都塞了**假数据**当占位
 * （`把 tools 那一层的 dispatch 拆出来…` 那几条）。那不是「还没接后端」，
 * 那是在界面上造了一段不存在的历史——用户分不清是真的跑过还是演示，
 * 出问题也复现不了。空就是空，明说。
 */
withDefaults(defineProps<{ title: string; hint?: string }>(), { hint: '' })
</script>

<template>
  <view class="empty">
    <text class="t">{{ title }}</text>
    <text v-if="hint" class="h">{{ hint }}</text>
    <slot />
  </view>
</template>

<style lang="scss" scoped>
.empty {
  display: flex;
  flex-direction: column;
  align-items: center;
  padding: 120rpx 48rpx;
}
.t {
  font-size: 26rpx;
  color: $fg2;
  text-align: center;
}
.h {
  font-size: 21rpx;
  color: $fg4;
  text-align: center;
  margin-top: 12rpx;
  line-height: 1.6;
}
</style>
