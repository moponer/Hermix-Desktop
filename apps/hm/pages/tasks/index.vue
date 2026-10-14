<script setup lang="ts">
/**
 * 任务页。
 *
 * 归属过滤是这一屏的**安全要点**：`GET /v1/tasks` 服务端已按
 * `origin.device_id()` 过滤（只回本设备的 sid），App 侧不要自己「帮忙」
 * 过滤——那等于把过滤逻辑复制一份，两边漂移就会漏。
 *
 * 每一位次都要显示：IM 侧原本的消息是挤聊天锁的，用户发完看不到任何反馈，
 * 轮后整批重跑。位次是「排队了」与「没收到」的唯一区别。
 *
 * 改造重点：原版塞了三条假任务和一条假的「上一轮没跑完」提示。现在是真空态。
 */
import { ref, computed } from 'vue'
import { onShow } from '@dcloudio/uni-app'
import { getTasks, interruptSession } from '../../core/tasks'

interface TaskView {
  sid: string
  state: 'running' | 'queued'
  first_line: string
  position: number | null
}

const tasks = ref<TaskView[]>([])
const loading = ref(false)

async function refresh() {
  loading.value = true
  const list = await getTasks()
  // 按 running 在前、queued 在后，再按 position 排序
  tasks.value = list.sort((a, b) => {
    if (a.state !== b.state) return a.state === 'running' ? -1 : 1
    if (a.state === 'queued' && b.state === 'queued') {
      return (a.position ?? 0) - (b.position ?? 0)
    }
    return 0
  })
  loading.value = false
}

async function stop(sid: string) {
  const res = await interruptSession(sid)
  if (res.interrupted) {
    uni.showToast({ title: '已中断', icon: 'none' })
    await refresh()
  } else {
    uni.showToast({ title: res.reason ?? '中断失败', icon: 'none' })
  }
}

const running = computed(() => tasks.value.filter((t) => t.state === 'running'))
const queued = computed(() => tasks.value.filter((t) => t.state === 'queued'))

onShow(() => {
  void refresh()
})
</script>

<template>
  <hm-shell title="任务">
    <template #end>
      <view class="icon-btn" @tap="refresh"><text>↻</text></view>
    </template>

    <hm-empty
      v-if="!tasks.length"
      :title="loading ? '正在读取任务…' : '当前没有任务'"
      hint="你在对话页下任务之后，运行中和排队的情况会出现在这里。"
    />

    <view v-for="(t, i) in tasks" :key="t.sid + i" class="task">
      <view class="lead">
        <view v-if="t.state === 'running'" class="spin" />
        <text v-else class="pos">第 {{ t.position }} 位</text>
      </view>
      <view class="txt">
        <text class="line">{{ t.first_line }}</text>
        <text class="sub">{{ t.state === 'running' ? '运行中' : '排队' }}</text>
      </view>
      <view v-if="t.state === 'running'" class="stop" @tap="stop(t.sid)"><text>■</text></view>
    </view>

    <view v-if="running.length || queued.length" class="foot">
      <text>共 {{ running.length }} 个运行中 · {{ queued.length }} 个排队</text>
    </view>
  </hm-shell>
</template>

<style lang="scss" scoped>
.icon-btn {
  width: $tap;
  height: $tap;
  margin: -16rpx -16rpx -16rpx 0;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: $r-pill;
  background: $raised;
  color: $fg;
  font-size: 32rpx;
}

/* 卡片比页底高一档，不描边。 */
.task {
  display: flex;
  align-items: center;
  gap: 18rpx;
  background: $card;
  border-radius: $r-card;
  padding: 24rpx;
  margin: 0 $gutter 16rpx;
}
.lead {
  flex: none;
  width: 64rpx;
  display: flex;
  justify-content: center;
}
.spin {
  width: 24rpx;
  height: 24rpx;
  border-radius: 50%;
  background: $accent;
  animation: pulse 1s ease-in-out infinite;
}
/* 不用 border + rotate 的菊花：暗底上那条 3rpx 描边几乎看不见，
   换成缩放脉冲，状态同样一目了然且没有描边。 */
@keyframes pulse {
  0%,
  100% {
    opacity: 0.35;
    transform: scale(0.82);
  }
  50% {
    opacity: 1;
    transform: scale(1);
  }
}
.pos {
  font-size: $fs-mini;
  color: $warn;
  background: $warn-bg;
  padding: 5rpx 12rpx;
  border-radius: $r-pill;
}
.txt {
  flex: 1;
  min-width: 0;
}
.line {
  display: block;
  font-size: $fs-body;
  color: $fg;
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}
.sub {
  display: block;
  font-size: $fs-mini;
  color: $fg4;
  margin-top: 4rpx;
}
.stop {
  flex: none;
  width: 72rpx;
  height: 72rpx;
  display: flex;
  align-items: center;
  justify-content: center;
  border-radius: 36rpx;
  background: $err-bg;
  color: $err;
  font-size: 26rpx;
}

.foot {
  padding: 12rpx $gutter 40rpx;
  font-size: $fs-mini;
  color: $fg4;
}
</style>
